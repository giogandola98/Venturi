use std::sync::Arc;

use crate::host::MemStream;
use vst3::Steinberg::Vst::IComponentHandlerTrait;
use vst3::Steinberg::kResultOk;
use vst3::{ComPtr, ComRef, ComWrapper};
use vv_audio::PluginProcessor;
use vv_core::{EffectId, PluginState, Vst3Effect};

use super::*;
use crate::testing::fixture_bundle;

struct CountingAlloc;

thread_local! {
    static ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// SAFETY: forwards to the system allocator, only counting on this thread.
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { std::alloc::System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static COUNTING: CountingAlloc = CountingAlloc;

#[test]
fn process_does_not_allocate() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    let mut audio = ramp(1300, 2);
    instance.process(&mut audio, 2);
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe { instance.handler.performEdit(0, 0.25) };
    let mut mono = ramp(700, 1);
    let before = ALLOCS.with(|c| c.get());
    for _ in 0..8 {
        instance.process(&mut audio, 2);
    }
    instance.process(&mut mono, 1);
    instance.reset();
    assert_eq!(ALLOCS.with(|c| c.get()) - before, 0);
}

#[test]
fn stream_seek_past_end_is_refused_and_read_is_empty() {
    use vst3::Steinberg::{IBStream, IBStreamTrait};
    let stream = MemStream::new(vec![1, 2, 3]);
    let s = stream.as_com_ref::<IBStream>().unwrap();
    let mut pos = 0;
    let mut buf = [0u8; 4];
    let mut read = -1;
    // SAFETY: valid out-pointers on a live stream.
    unsafe {
        assert_ne!(s.seek(10, 0, &mut pos), kResultOk);
        assert_eq!(s.seek(3, 0, &mut pos), kResultOk);
        assert_eq!(s.read(buf.as_mut_ptr().cast(), 4, &mut read), kResultOk);
    }
    assert_eq!(read, 0);
}

#[test]
fn host_creates_messages_with_round_tripping_attributes() {
    use std::ffi::{CString, c_void};
    use vst3::Steinberg::Vst::{
        IAttributeListTrait, IHostApplication, IHostApplicationTrait, IMessage, IMessage_iid,
        IMessageTrait,
    };
    let host = ComWrapper::new(crate::host::HostApp);
    let app = host.as_com_ref::<IHostApplication>().unwrap();
    let mut cid = IMessage_iid;
    let mut obj: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid ids and out-pointers; the object is released by the ComPtr.
    unsafe {
        assert_eq!(app.createInstance(&mut cid, &mut cid, &mut obj), kResultOk);
        let msg = ComPtr::from_raw(obj.cast::<IMessage>()).unwrap();
        let id = CString::new("hello").unwrap();
        msg.setMessageID(id.as_ptr());
        assert_eq!(std::ffi::CStr::from_ptr(msg.getMessageID()), id.as_c_str());
        let attrs = ComRef::from_raw(msg.getAttributes()).unwrap();
        let key = |s: &str| CString::new(s).unwrap();
        attrs.setInt(key("i").as_ptr(), -7);
        attrs.setFloat(key("f").as_ptr(), 2.5);
        let text: Vec<u16> = "ciao\0".encode_utf16().collect();
        attrs.setString(key("s").as_ptr(), text.as_ptr());
        attrs.setBinary(key("b").as_ptr(), [9u8, 8, 7].as_ptr().cast(), 3);

        let (mut i, mut f) = (0i64, 0.0f64);
        assert_eq!(attrs.getInt(key("i").as_ptr(), &mut i), kResultOk);
        assert_eq!(attrs.getFloat(key("f").as_ptr(), &mut f), kResultOk);
        assert_eq!((i, f), (-7, 2.5));
        let mut out = [0u16; 16];
        assert_eq!(
            attrs.getString(key("s").as_ptr(), out.as_mut_ptr(), 32),
            kResultOk
        );
        assert_eq!(out[..5], text[..]);
        let (mut data, mut len): (*const c_void, u32) = (std::ptr::null(), 0);
        assert_eq!(
            attrs.getBinary(key("b").as_ptr(), &mut data, &mut len),
            kResultOk
        );
        assert_eq!(
            std::slice::from_raw_parts(data.cast::<u8>(), len as usize),
            [9, 8, 7]
        );
        assert_ne!(attrs.getInt(key("missing").as_ptr(), &mut i), kResultOk);
    }
}

#[test]
fn set_state_discards_pending_edits() {
    let instance = fixture(Vec::new());
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe { instance.handler.performEdit(0, 0.25) };
    instance.set_state(&effect(Vec::new()));
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 1.0);
}

fn ramp(frames: usize, channels: usize) -> Vec<f32> {
    (0..frames * channels)
        .map(|i| i as f32 * 0.001 + 0.1)
        .collect()
}

fn scratch() -> [Vec<f32>; 2] {
    [vec![9.0; 512], vec![9.0; 512]]
}

#[test]
fn deinterleave_round_trip_stereo() {
    let input = ramp(5, 2);
    let mut planar = scratch();
    deinterleave(&input, 2, &mut planar, 5);
    assert_eq!(planar[0][..5], [0.1, 0.102, 0.104, 0.106, 0.108]);
    assert_eq!(planar[1][0], 0.101);
    let mut out = vec![0.0; 10];
    interleave(&planar, 2, &mut out, 5);
    assert_eq!(out, input);
}

#[test]
fn interleave_maps_mono_and_surround() {
    let mono = ramp(4, 1);
    let mut planar = scratch();
    deinterleave(&mono, 1, &mut planar, 4);
    assert_eq!(planar[0][..4], planar[1][..4]);
    planar[0][..4].fill(0.5);
    planar[1][..4].fill(0.7);
    let mut out = mono.clone();
    interleave(&planar, 1, &mut out, 4);
    assert_eq!(out, [0.5; 4], "mono takes the left output");

    let surround = ramp(3, 6);
    deinterleave(&surround, 6, &mut planar, 3);
    assert_eq!(planar[0][1], surround[6]);
    assert_eq!(planar[1][1], surround[7]);
    planar[0][..3].fill(-1.0);
    planar[1][..3].fill(-2.0);
    let mut out = surround.clone();
    interleave(&planar, 6, &mut out, 3);
    for frame in out.chunks(6).zip(surround.chunks(6)) {
        assert_eq!(frame.0[..2], [-1.0, -2.0]);
        assert_eq!(frame.0[2..], frame.1[2..], "channels 2-5 are untouched");
    }
}

fn effect(state: Vec<u8>) -> Vst3Effect {
    effect_of("56565445535447414E00000000000001", state)
}

fn effect_of(class_id: &str, state: Vec<u8>) -> Vst3Effect {
    Vst3Effect {
        id: EffectId(1),
        class_id: class_id.into(),
        name: "Venturi Test Gain".into(),
        vendor: "Venturi".into(),
        version: "1.0.0".into(),
        component_state: PluginState::new(state),
        controller_state: PluginState::new(Vec::new()),
    }
}

fn fixture(state: Vec<u8>) -> Arc<Vst3Instance> {
    Vst3Instance::new(
        &fixture_bundle(),
        &effect(state),
        48_000,
        ProcessMode::Realtime,
    )
    .expect("fixture instance")
}

fn run(instance: &Vst3Instance, input: &[f32]) -> Vec<f32> {
    let mut out = input.to_vec();
    instance.process(&mut out, 2);
    out
}

fn assert_scaled(out: &[f32], input: &[f32], gain: f32) {
    for (o, i) in out.iter().zip(input) {
        assert!((o - i * gain).abs() < 1e-6, "{o} vs {i} * {gain}");
    }
}

#[test]
fn fixture_gain_applies() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    let input = ramp(512, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
    // Longer than one plugin block: processed in slices.
    let input = ramp(1300, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn fixture_default_state_is_unity() {
    let instance = fixture(Vec::new());
    let input = ramp(100, 2);
    assert_scaled(&run(&instance, &input), &input, 1.0);
}

#[test]
fn fixture_state_round_trips() {
    let bytes = 0.25f32.to_le_bytes().to_vec();
    let instance = fixture(Vec::new());
    instance.set_state(&effect(bytes.clone()));
    let (component, _) = instance.get_state();
    assert_eq!(component.bytes(), bytes);
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn fixture_param_change_reaches_processor() {
    let instance = fixture(Vec::new());
    let input = ramp(256, 2);
    assert_scaled(&run(&instance, &input), &input, 1.0);
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance.handler.beginEdit(0);
        instance.handler.performEdit(0, 0.25);
        instance.handler.endEdit(0);
    }
    assert!(instance.take_dirty());
    assert!(!instance.take_dirty());
    assert_scaled(&run(&instance, &input), &input, 0.5);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

fn split_fixture() -> Arc<Vst3Instance> {
    let effect = effect_of("56565445535447414E00000000000002", Vec::new());
    Vst3Instance::new(&fixture_bundle(), &effect, 48_000, ProcessMode::Realtime)
        .expect("split fixture instance")
}

#[test]
fn plugin_whose_set_processing_fails_still_loads_and_processes() {
    for class_id in [
        "56565445535447414E00000000000004", // kNotImplemented
        "56565445535447414E00000000000005", // kResultFalse
    ] {
        let effect = effect_of(class_id, Vec::new());
        let instance = Vst3Instance::new(&fixture_bundle(), &effect, 48_000, ProcessMode::Realtime)
            .expect("setProcessing's result is informational");
        instance.reset();
        let input = ramp(64, 2);
        assert_scaled(&run(&instance, &input), &input, 1.0);
    }
}

#[test]
fn get_state_flushes_pending_edits_to_a_split_processor() {
    let instance = split_fixture();
    assert!(instance.separate_controller);
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance.handler.beginEdit(0);
        instance.handler.performEdit(0, 0.25);
        instance.handler.endEdit(0);
    }
    assert!(instance.take_dirty());
    let (component, _) = instance.get_state();
    assert_eq!(component.bytes(), 0.25f32.to_le_bytes());
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn get_state_flush_does_not_replay_edits_on_the_next_process() {
    let instance = split_fixture();
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe { instance.handler.performEdit(0, 0.25) };
    instance.get_state();
    assert!(instance.shared.params.lock().unwrap().is_empty());
}

#[test]
fn busy_instance_passes_audio_through() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    instance
        .busy
        .store(true, std::sync::atomic::Ordering::Release);
    let input = ramp(64, 2);
    assert_eq!(run(&instance, &input), input);
    instance
        .busy
        .store(false, std::sync::atomic::Ordering::Release);
}

#[test]
fn mono_and_surround_run_through_the_plugin() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    let mut mono = ramp(32, 1);
    let want: Vec<f32> = mono.iter().map(|s| s * 0.5).collect();
    instance.process(&mut mono, 1);
    assert_eq!(mono.len(), want.len());
    for (a, b) in mono.iter().zip(&want) {
        assert!((a - b).abs() < 1e-6);
    }
    let surround = ramp(32, 6);
    let mut out = surround.clone();
    instance.process(&mut out, 6);
    for (o, i) in out.chunks(6).zip(surround.chunks(6)) {
        assert!((o[0] - i[0] * 0.5).abs() < 1e-6);
        assert_eq!(o[2..], i[2..]);
    }
}

#[test]
fn unknown_class_is_an_error() {
    let mut bad = effect(Vec::new());
    bad.class_id = "00000000000000000000000000000000".into();
    let r = Vst3Instance::new(&fixture_bundle(), &bad, 48_000, ProcessMode::Offline);
    assert!(r.is_err());
    bad.class_id = "zz".into();
    assert!(Vst3Instance::new(&fixture_bundle(), &bad, 48_000, ProcessMode::Offline).is_err());
}

#[cfg(target_os = "macos")]
mod real_plugins {
    use super::*;
    use crate::scan;
    use std::path::PathBuf;

    fn installed(name: &str) -> Option<(PathBuf, Vst3Effect)> {
        let dir = PathBuf::from("/Library/Audio/Plug-Ins/VST3");
        let info = scan(&[dir]).into_iter().find(|p| p.name.contains(name))?;
        let mut e = effect(Vec::new());
        e.class_id = info.class_id;
        e.name = info.name;
        Some((info.path, e))
    }

    fn noise(frames: usize) -> Vec<f32> {
        let mut x = 0x1234_5678u32;
        (0..frames * 2)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    }

    #[test]
    #[ignore = "needs Wider installed"]
    fn wider_processes_noise_with_finite_output() {
        let (path, e) = installed("Wider").expect("Wider installed");
        let inst = Vst3Instance::new(&path, &e, 48_000, ProcessMode::Realtime).unwrap();
        let mut audio = noise(48_000);
        for block in audio.chunks_mut(512 * 2) {
            inst.process(block, 2);
        }
        assert!(audio.iter().all(|s| s.is_finite()));
    }

    // TDR writes a per-process instance counter into its state.
    fn mask_instance(mut state: Vec<u8>) -> Vec<u8> {
        let key = b"instance=\"";
        if let Some(at) = state.windows(key.len()).position(|w| w == key) {
            let end = (at + key.len() + 16).min(state.len());
            state[at + key.len()..end].fill(0);
        }
        state
    }

    #[test]
    #[ignore = "needs TDR Nova installed"]
    fn state_round_trips() {
        let (path, e) = installed("TDR Nova").expect("TDR Nova installed");
        let a = Vst3Instance::new(&path, &e, 48_000, ProcessMode::Realtime).unwrap();
        let (c1, k1) = a.get_state();
        assert!(!c1.bytes().is_empty());
        let mut saved = e.clone();
        saved.component_state = c1.clone();
        saved.controller_state = k1.clone();
        let b = Vst3Instance::new(&path, &saved, 48_000, ProcessMode::Realtime).unwrap();
        let (c2, k2) = b.get_state();
        assert_eq!(
            mask_instance(c1.bytes().to_vec()),
            mask_instance(c2.bytes().to_vec())
        );
        assert_eq!(k1, k2);
    }

    #[test]
    #[ignore = "needs TDR Nova installed"]
    fn nova_default_is_near_transparent() {
        let (path, e) = installed("TDR Nova").expect("TDR Nova installed");
        let inst = Vst3Instance::new(&path, &e, 48_000, ProcessMode::Realtime).unwrap();
        let latency = inst.latency() as usize;
        let input = noise(48_000);
        let mut out = input.clone();
        for block in out.chunks_mut(512 * 2) {
            inst.process(block, 2);
        }
        // Compare after the plugin's own delay, skipping the settle time.
        let skip = 8192 * 2;
        let (mut err, mut sig) = (0.0f64, 0.0f64);
        for i in skip..input.len() - latency * 2 {
            let d = f64::from(out[i + latency * 2] - input[i]);
            err += d * d;
            sig += f64::from(input[i]).powi(2);
        }
        let db = 10.0 * (err / sig).log10();
        assert!(db < -90.0, "residual {db} dB");
    }
}
