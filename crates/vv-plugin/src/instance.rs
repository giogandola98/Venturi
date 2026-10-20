//! A hosted VST3 effect, driven with the mixer's interleaved blocks.
use std::cell::UnsafeCell;
use std::ffi::c_char;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, TryLockError};

use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, IAudioProcessor, IAudioProcessorTrait, IComponent,
    IComponentHandler, IComponentTrait, IConnectionPoint, IConnectionPointTrait, IEditController,
    IEditControllerTrait, IParameterChanges, ProcessContext, ProcessData, ProcessSetup,
    RestartFlags_, SpeakerArr, SpeakerArrangement,
};
use vst3::Steinberg::{FUnknown, IBStream, IPluginBaseTrait, TUID, kResultOk};
use vst3::{ComPtr, ComWrapper};
use vv_audio::{MIX_BLOCK_FRAMES, PluginProcessor};
use vv_core::{PluginState, Vst3Effect};

use crate::HostedPlugin;
use crate::host::{ComponentHandler, HostApp, MAX_PARAMS, MemStream, ParamChanges, Shared};
use crate::module::{Module, module};

// The generated enum constants are u32 on some targets and i32 on others.
#[allow(clippy::unnecessary_cast)]
mod ids {
    use vst3::Steinberg::Vst::{
        BusDirections_, MediaTypes_, ProcessContext_, ProcessModes_, SymbolicSampleSizes_,
    };
    pub const INPUT: i32 = BusDirections_::kInput as i32;
    pub const OUTPUT: i32 = BusDirections_::kOutput as i32;
    pub const AUDIO: i32 = MediaTypes_::kAudio as i32;
    pub const REALTIME: i32 = ProcessModes_::kRealtime as i32;
    pub const OFFLINE: i32 = ProcessModes_::kOffline as i32;
    pub const TEMPO_VALID: u32 = ProcessContext_::StatesAndFlags_::kTempoValid as u32;
    pub const TIME_SIG_VALID: u32 = ProcessContext_::StatesAndFlags_::kTimeSigValid as u32;
    pub const SAMPLE32: i32 = SymbolicSampleSizes_::kSample32 as i32;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcessMode {
    Realtime,
    Offline,
}

impl ProcessMode {
    // One mapping for setup and every process call: VST3 requires them to agree.
    fn sdk(self) -> i32 {
        match self {
            ProcessMode::Realtime => ids::REALTIME,
            ProcessMode::Offline => ids::OFFLINE,
        }
    }
}

/// Everything the audio thread touches; guarded by `Vst3Instance::busy`.
struct Rt {
    inp: [Vec<f32>; 2],
    out: [Vec<f32>; 2],
    changes: ComWrapper<ParamChanges>,
    changes_ptr: ComPtr<IParameterChanges>,
    outputs: ComWrapper<ParamChanges>,
    outputs_ptr: ComPtr<IParameterChanges>,
    context: ProcessContext,
}

pub struct Vst3Instance {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    // Only a separate controller object is initialized and terminated by us.
    separate_controller: bool,
    connections: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    // Drop only undoes what succeeded: VST3 forbids deactivating a plugin never activated.
    active: bool,
    processing: bool,
    pub(crate) handler: ComPtr<IComponentHandler>,
    host: ComPtr<FUnknown>,
    shared: Arc<Shared>,
    rt: UnsafeCell<Rt>,
    busy: AtomicBool,
    mode: ProcessMode,
    state_hash: AtomicU64,
    latency: AtomicU32,
    // Read before any state is loaded: what an empty saved state means.
    defaults: (Vec<u8>, Vec<u8>),
    // Last: fields drop in declaration order, and the plugin objects above
    // must be released while the module's code is still loaded.
    _module: Arc<Module>,
}

// SAFETY: `rt` is only used by whoever holds `busy`; the COM pointers are to
// objects VST3 requires to be callable from the threads we use them on.
unsafe impl Send for Vst3Instance {}
unsafe impl Sync for Vst3Instance {}

/// Exclusive use of `Vst3Instance::rt`; releases `busy` on every exit, panics included.
struct AudioGuard<'a>(&'a AtomicBool);

impl<'a> AudioGuard<'a> {
    fn try_acquire(busy: &'a AtomicBool) -> Option<Self> {
        busy.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self(busy))
    }
}

impl Drop for AudioGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn parse_cid(hex: &str) -> Result<TUID, String> {
    let bad = || format!("bad class id {hex:?}");
    if hex.len() != 32 || !hex.is_ascii() {
        return Err(bad());
    }
    let mut cid: TUID = [0; 16];
    for (byte, pair) in cid.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).map_err(|_| bad())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| bad())? as c_char;
    }
    Ok(cid)
}

fn ok(res: i32, what: &str) -> Result<(), String> {
    (res == kResultOk)
        .then_some(())
        .ok_or_else(|| format!("{what} failed ({res})"))
}

impl Vst3Instance {
    /// On a non-realtime thread. Stereo in/out, blocks of at most 512 frames, f32.
    /// Drop it on the UI thread too: teardown calls into the plugin.
    pub fn new(
        info_path: &Path,
        effect: &Vst3Effect,
        sample_rate: u32,
        mode: ProcessMode,
    ) -> Result<Arc<Self>, String> {
        let module = module(info_path)?;
        let cid = parse_cid(&effect.class_id)?;
        let component = module
            .create::<IComponent>(&cid)
            .ok_or("the plugin could not create its component")?;
        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or("the plugin has no audio processor")?;

        let shared = Arc::new(Shared::new());
        let host = ComWrapper::new(HostApp)
            .to_com_ptr::<FUnknown>()
            .ok_or("host context")?;
        let handler = ComWrapper::new(ComponentHandler(shared.clone()))
            .to_com_ptr::<IComponentHandler>()
            .ok_or("component handler")?;
        let changes = ParamChanges::input();
        let outputs = ParamChanges::output();
        let rt = Rt {
            inp: [vec![0.0; MIX_BLOCK_FRAMES], vec![0.0; MIX_BLOCK_FRAMES]],
            out: [vec![0.0; MIX_BLOCK_FRAMES], vec![0.0; MIX_BLOCK_FRAMES]],
            changes_ptr: changes.to_com_ptr().ok_or("parameter changes")?,
            outputs_ptr: outputs.to_com_ptr().ok_or("parameter changes")?,
            outputs,
            changes,
            // SAFETY: a plain C struct; zero is a valid empty state.
            context: unsafe { std::mem::zeroed() },
        };

        // SAFETY: a live component and a host context that outlives it.
        ok(unsafe { component.initialize(host.as_ptr()) }, "initialize")?;
        // From here Drop undoes whatever was done, error or not.
        let mut this = Self {
            _module: module.clone(),
            component,
            processor,
            controller: None,
            separate_controller: false,
            connections: None,
            active: false,
            processing: false,
            handler,
            host,
            shared,
            rt: UnsafeCell::new(rt),
            busy: AtomicBool::new(false),
            mode,
            state_hash: AtomicU64::new(effect.content_hash()),
            latency: AtomicU32::new(0),
            defaults: (Vec::new(), Vec::new()),
        };
        this.attach_controller(&module)?;
        this.setup(sample_rate)?;
        this.defaults = this.read_state();
        if !effect.component_state.bytes().is_empty() || !effect.controller_state.bytes().is_empty()
        {
            this.load_state(&effect.component_state, &effect.controller_state);
        }
        // SAFETY: setup is complete; these start the processing the mixer drives.
        unsafe {
            ok(this.component.setActive(1), "setActive")?;
            this.active = true;
            // Informational: the SDK default returns kNotImplemented.
            this.processor.setProcessing(1);
            this.processing = true;
        }
        this.refresh_latency_after_activation();
        Ok(Arc::new(this))
    }

    fn attach_controller(&mut self, module: &Module) -> Result<(), String> {
        let mut class_id: TUID = [0; 16];
        // SAFETY: out-parameter call on a live component.
        let separate = unsafe { self.component.getControllerClassId(&mut class_id) } == kResultOk
            && class_id.iter().any(|&b| b != 0);
        if !separate {
            self.controller = self.component.cast::<IEditController>();
        } else {
            let controller = module
                .create::<IEditController>(&class_id)
                .ok_or("the plugin could not create its controller")?;
            // SAFETY: a live controller and host context.
            ok(
                unsafe { controller.initialize(self.host.as_ptr()) },
                "controller initialize",
            )?;
            self.separate_controller = true;
            self.controller = Some(controller);
            let pair = self.component.cast::<IConnectionPoint>().zip(
                self.controller
                    .as_ref()
                    .and_then(|c| c.cast::<IConnectionPoint>()),
            );
            if let Some((a, b)) = pair {
                // SAFETY: both ends are live; each is told about the other.
                unsafe {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                }
                self.connections = Some((a, b));
            }
        }
        if let Some(controller) = &self.controller {
            // SAFETY: the handler outlives the controller's use of it (cleared in Drop).
            unsafe { controller.setComponentHandler(self.handler.as_ptr()) };
        }
        Ok(())
    }

    fn setup(&self, sample_rate: u32) -> Result<(), String> {
        let mut stereo = [SpeakerArr::kStereo];
        let mut arranged = [0 as SpeakerArrangement; 2];
        // SAFETY: calls on live interfaces with arrays of the lengths passed.
        unsafe {
            // A rejection is fine if the default arrangement is already stereo.
            self.processor
                .setBusArrangements(stereo.as_mut_ptr(), 1, stereo.as_mut_ptr(), 1);
            let dirs = [ids::INPUT, ids::OUTPUT];
            for (arr, dir) in arranged.iter_mut().zip(dirs) {
                let res = self.processor.getBusArrangement(dir, 0, arr);
                if res != kResultOk || *arr != SpeakerArr::kStereo {
                    return Err("the plugin does not support a stereo bus".into());
                }
                // Not every plugin enables its buses by default.
                self.component.activateBus(ids::AUDIO, dir, 0, 1);
            }
            ok(
                self.processor.canProcessSampleSize(ids::SAMPLE32),
                "32-bit processing",
            )?;
            let mut setup = ProcessSetup {
                processMode: self.mode.sdk(),
                symbolicSampleSize: ids::SAMPLE32,
                maxSamplesPerBlock: MIX_BLOCK_FRAMES as i32,
                sampleRate: f64::from(sample_rate),
            };
            ok(
                self.processor.setupProcessing(&mut setup),
                "setupProcessing",
            )?;
            // Tempo-synced plugins need something sane; there is no transport to follow.
            let context = &mut (*self.rt.get()).context;
            context.sampleRate = f64::from(sample_rate);
            context.tempo = 120.0;
            context.timeSigNumerator = 4;
            context.timeSigDenominator = 4;
            context.state = ids::TEMPO_VALID | ids::TIME_SIG_VALID;
        }
        Ok(())
    }

    // A plugin that rejects a state keeps its defaults: not worth failing the insert.
    fn load_state(&self, saved_component: &PluginState, saved_controller: &PluginState) {
        let stream = |bytes: &[u8]| MemStream::new(bytes.to_vec());
        // An empty half is the plugin's defaults, not "leave as is".
        let half = |saved: &PluginState, default: &[u8]| -> Vec<u8> {
            let bytes = saved.bytes();
            if bytes.is_empty() {
                default.to_vec()
            } else {
                bytes.to_vec()
            }
        };
        let component = &half(saved_component, &self.defaults.0);
        let controller = &half(saved_controller, &self.defaults.1);
        // SAFETY: the streams outlive each call; the plugin copies what it needs.
        unsafe {
            if !component.is_empty() {
                let s = stream(component);
                if let Some(p) = s.to_com_ptr::<IBStream>() {
                    self.component.setState(p.as_ptr());
                }
                // A single object already took the state through its component half.
                let s = stream(component);
                if let (Some(c), Some(p), true) = (
                    &self.controller,
                    s.to_com_ptr::<IBStream>(),
                    self.separate_controller,
                ) {
                    c.setComponentState(p.as_ptr());
                }
            }
            if !controller.is_empty() {
                let s = stream(controller);
                if let (Some(c), Some(p)) = (&self.controller, s.to_com_ptr::<IBStream>()) {
                    c.setState(p.as_ptr());
                }
            }
        }
    }

    fn try_lock_audio(&self) -> Option<AudioGuard<'_>> {
        AudioGuard::try_acquire(&self.busy)
    }

    fn lock_audio(&self) -> AudioGuard<'_> {
        loop {
            if let Some(guard) = self.try_lock_audio() {
                return guard;
            }
            std::thread::yield_now();
        }
    }

    /// Every restart flag raised while activating is dropped on purpose: the
    /// instance was just (re)activated with its current state, and keeping one
    /// pending would reactivate a running instance (or loop, if raised each time).
    fn refresh_latency_after_activation(&self) {
        self.shared.restart.store(0, Ordering::Release);
        self.refresh_latency();
    }

    fn refresh_latency(&self) {
        // SAFETY: a live processor.
        let samples = unsafe { self.processor.getLatencySamples() };
        self.latency.store(samples, Ordering::Relaxed);
    }

    /// Queued edits reach the processor first (a VST3 parameter flush), so the
    /// component state includes them even when no audio is running.
    pub fn get_state(&self) -> (PluginState, PluginState) {
        self.flush_params();
        let (component, controller) = self.read_state();
        (PluginState::new(component), PluginState::new(controller))
    }

    fn read_state(&self) -> (Vec<u8>, Vec<u8>) {
        let save = |write: &dyn Fn(*mut IBStream)| {
            let stream = MemStream::new(Vec::new());
            if let Some(p) = stream.to_com_ptr::<IBStream>() {
                write(p.as_ptr());
            }
            stream.take()
        };
        // SAFETY: the stream is live for the call.
        let component = save(&|s| unsafe {
            self.component.getState(s);
        });
        let controller = save(&|s| unsafe {
            if let Some(c) = &self.controller {
                c.getState(s);
            }
        });
        (component, controller)
    }

    fn flush_params(&self) {
        let audio = self.lock_audio();
        self.flush_params_locked(&audio);
    }

    fn flush_params_locked(&self, _audio: &AudioGuard<'_>) {
        // SAFETY: the guard gives this thread exclusive use of `rt`.
        let rt = unsafe { &mut *self.rt.get() };
        // Bounded so a plugin that keeps queueing edits from inside `process` cannot spin us.
        let mut slices = None;
        loop {
            rt.changes.clear();
            {
                let mut params = self.shared.params.lock().unwrap_or_else(|e| e.into_inner());
                let left = slices.get_or_insert(params.len().div_ceil(MAX_PARAMS) + 1);
                if params.is_empty() || *left == 0 {
                    break;
                }
                *left -= 1;
                let n = params.len().min(MAX_PARAMS);
                for (id, value) in params.drain(..n) {
                    rt.changes.push(id, value);
                }
            }
            self.run_slice(rt, 0);
        }
        rt.changes.clear();
    }

    pub(crate) fn controller(&self) -> Option<&ComPtr<IEditController>> {
        self.controller.as_ref()
    }

    pub(crate) fn set_waker(&self, waker: Option<crate::host::Waker>) {
        self.shared.set_waker(waker);
    }

    /// An edit gesture ended since the last call.
    pub fn take_dirty(&self) -> bool {
        self.shared.dirty.swap(false, Ordering::AcqRel)
    }

    /// Audio thread: never blocks, a contended block is skipped.
    fn publish_outputs(&self, outputs: &ParamChanges) {
        let Ok(mut latest) = self.shared.outputs.try_lock() else {
            return;
        };
        let mut fresh = false;
        outputs.for_each_value(|id, value| {
            if let Some(slot) = latest.iter_mut().find(|p| p.0 == id) {
                if slot.1 != value {
                    *slot = (id, value, true);
                    fresh = true;
                }
            } else if latest.len() < MAX_PARAMS {
                latest.push((id, value, true));
                fresh = true;
            }
        });
        if fresh {
            self.shared.outputs_fresh.store(true, Ordering::Release);
        }
    }

    /// Hands the plugin's changed output parameters to its controller; UI thread only.
    pub(crate) fn forward_outputs(&self) {
        let Some(controller) = &self.controller else {
            return;
        };
        if !self.shared.outputs_fresh.load(Ordering::Acquire) {
            return;
        }
        let mut changed = [(0, 0.0); MAX_PARAMS];
        let mut n = 0;
        {
            let mut latest = self
                .shared
                .outputs
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.shared.outputs_fresh.store(false, Ordering::Release);
            for slot in latest.iter_mut().filter(|p| p.2) {
                slot.2 = false;
                changed[n] = (slot.0, slot.1);
                n += 1;
            }
        }
        for &(id, value) in &changed[..n] {
            // SAFETY: a live controller, called from the UI thread.
            unsafe { controller.setParamNormalized(id, value) };
        }
    }

    /// Output parameters changed since `forward_outputs` last ran.
    pub(crate) fn outputs_fresh(&self) -> bool {
        self.shared.outputs_fresh.load(Ordering::Acquire)
    }

    fn run_slice(&self, rt: &mut Rt, frames: usize) {
        let mut ins = [rt.inp[0].as_mut_ptr(), rt.inp[1].as_mut_ptr()];
        let mut outs = [rt.out[0].as_mut_ptr(), rt.out[1].as_mut_ptr()];
        let mut input = AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: ins.as_mut_ptr(),
            },
        };
        let mut output = AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: outs.as_mut_ptr(),
            },
        };
        let mut data = ProcessData {
            processMode: self.mode.sdk(),
            symbolicSampleSize: ids::SAMPLE32,
            numSamples: frames as i32,
            numInputs: 1,
            numOutputs: 1,
            inputs: &mut input,
            outputs: &mut output,
            inputParameterChanges: rt.changes_ptr.as_ptr(),
            outputParameterChanges: rt.outputs_ptr.as_ptr(),
            inputEvents: std::ptr::null_mut(),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut rt.context,
        };
        rt.outputs.clear();
        // SAFETY: buffers hold `frames` samples each; `data` lives for the call.
        let res = unsafe { self.processor.process(&mut data) };
        self.publish_outputs(&rt.outputs);
        if res != kResultOk {
            for c in 0..2 {
                rt.out[c][..frames].copy_from_slice(&rt.inp[c][..frames]);
            }
        }
    }
}

impl HostedPlugin for Vst3Instance {
    fn into_vst3(self: Arc<Self>) -> Option<Arc<Vst3Instance>> {
        Some(self)
    }

    /// Waits for the audio thread to leave `process`; UI thread only.
    fn set_state(&self, effect: &Vst3Effect) {
        let _audio = self.lock_audio();
        // A stale edit must not override the state just loaded.
        self.shared
            .params
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.load_state(&effect.component_state, &effect.controller_state);
        self.refresh_latency();
        self.mark_state(effect.content_hash());
    }

    fn state_hash(&self) -> u64 {
        self.state_hash.load(Ordering::Relaxed)
    }

    fn mark_state(&self, hash: u64) {
        self.state_hash.store(hash, Ordering::Relaxed);
    }

    /// The deactivate/activate cycle the spec requires; kIoChanged needs
    /// nothing more as the buses are fixed stereo.
    fn service_restart(&self) {
        if self.shared.restart.load(Ordering::Acquire) == 0 {
            return;
        }
        let flags = self.shared.restart.swap(0, Ordering::AcqRel);
        let audio = self.lock_audio();
        let saved = (flags & RestartFlags_::kReloadComponent != 0).then(|| {
            self.flush_params_locked(&audio);
            self.read_state()
        });
        // SAFETY: a live processor and component, from the UI thread; the busy
        // guard keeps the audio thread out while processing is stopped.
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            if let Some((component, controller)) = saved {
                // The flush above delivered the queued edits; drop what arrived since.
                self.shared
                    .params
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                self.load_state(&PluginState::new(component), &PluginState::new(controller));
            }
            self.component.setActive(1);
            self.processor.setProcessing(1);
        }
        self.refresh_latency_after_activation();
    }

    fn reactivate(&self) {
        let _audio = self.lock_audio();
        // SAFETY: as in `service_restart`.
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            self.component.setActive(1);
            self.processor.setProcessing(1);
        }
        self.refresh_latency_after_activation();
    }

    fn take_gesture(&self) -> Option<(PluginState, PluginState)> {
        self.take_dirty().then(|| self.get_state())
    }
}

impl PluginProcessor for Vst3Instance {
    fn process(&self, samples: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let Some(_audio) = self.try_lock_audio() else {
            return;
        };
        // SAFETY: `busy` gives this thread exclusive use of `rt`.
        let rt = unsafe { &mut *self.rt.get() };
        rt.changes.clear();
        match self.shared.params.try_lock() {
            Ok(mut params) => {
                let n = params.len().min(MAX_PARAMS);
                for (id, value) in params.drain(..n) {
                    rt.changes.push(id, value);
                }
            }
            Err(TryLockError::Poisoned(e)) => e.into_inner().clear(),
            Err(TryLockError::WouldBlock) => {}
        }
        let total = samples.len() / channels;
        for start in (0..total).step_by(MIX_BLOCK_FRAMES) {
            let frames = MIX_BLOCK_FRAMES.min(total - start);
            let block = &mut samples[start * channels..(start + frames) * channels];
            deinterleave(block, channels, &mut rt.inp, frames);
            self.run_slice(rt, frames);
            interleave(&rt.out, channels, block, frames);
            // Changes go to the first slice only.
            rt.changes.clear();
        }
    }

    fn reset(&self) {
        let Some(_audio) = self.try_lock_audio() else {
            return;
        };
        // SAFETY: restarting processing is allowed from the audio thread.
        unsafe {
            self.processor.setProcessing(0);
            self.processor.setProcessing(1);
        }
    }

    fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        // SAFETY: tears down in reverse of setup on live interfaces.
        unsafe {
            if self.processing {
                self.processor.setProcessing(0);
            }
            if self.active {
                self.component.setActive(0);
            }
            if let Some(c) = &self.controller {
                c.setComponentHandler(std::ptr::null_mut());
            }
            if let Some((a, b)) = &self.connections {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = self
                .controller
                .as_ref()
                .filter(|_| self.separate_controller)
            {
                c.terminate();
            }
            self.component.terminate();
        }
    }
}

/// Mono feeds both plugin inputs; more than two channels: only the first two are read (`PluginEffect` refills the rest).
pub(crate) fn deinterleave(
    samples: &[f32],
    channels: usize,
    out: &mut [Vec<f32>; 2],
    frames: usize,
) {
    let right = usize::from(channels > 1);
    for (n, frame) in samples.chunks_exact(channels).take(frames).enumerate() {
        out[0][n] = frame[0];
        out[1][n] = frame[right];
    }
}

pub(crate) fn interleave(inp: &[Vec<f32>; 2], channels: usize, samples: &mut [f32], frames: usize) {
    for (n, frame) in samples.chunks_exact_mut(channels).take(frames).enumerate() {
        frame[0] = inp[0][n];
        if channels > 1 {
            frame[1] = inp[1][n];
        }
    }
}

#[cfg(test)]
#[path = "tests/instance.rs"]
mod tests;
