//! A stereo gain VST3 used as a host test fixture. The VST3 API is raw FFI, so
//! the trait impls are unsafe by nature; nothing here may panic across it.
use std::ffi::{CString, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper};

const PLUGIN_NAME: &str = "Venturi Test Gain";
const VENDOR: &str = "Venturi";
// ASCII "VVTESTGAN" then zeros and a trailing 1: hex 56565445535447414E00000000000001.
const CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 1,
];
// Split variant: a processor-only component and its separate controller object.
const SPLIT_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 2,
];
const SPLIT_CONTROLLER_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 3,
];
// Single-component variant whose setProcessing returns kNotImplemented, like the SDK default.
const NOT_IMPL_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 4,
];
const DEFAULT_NORMALIZED: f64 = 0.5;

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let bytes = CString::new(src).unwrap_or_default();
    let bytes = bytes.as_bytes_with_nul();
    let n = bytes.len().min(dst.len());
    for (d, s) in dst.iter_mut().zip(&bytes[..n]) {
        *d = *s as c_char;
    }
    if let Some(last) = dst.get_mut(n.saturating_sub(1)) {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
        *d = s as TChar;
        len += 1;
    }
    if let Some(end) = dst.get_mut(len.min(dst.len().saturating_sub(1))) {
        *end = 0;
    }
}

fn gain(normalized: f64) -> f32 {
    (normalized.clamp(0.0, 1.0) * 2.0) as f32
}

struct Gain {
    // Normalized value as f64 bits, shared by the processor and controller halves
    // (in the split variant each object has its own copy).
    normalized: AtomicU64,
    controller_cid: Option<TUID>,
    set_processing_result: tresult,
}

impl Gain {
    fn new(controller_cid: Option<TUID>) -> Self {
        Self {
            normalized: AtomicU64::new(DEFAULT_NORMALIZED.to_bits()),
            controller_cid,
            set_processing_result: kResultOk,
        }
    }

    fn get(&self) -> f64 {
        f64::from_bits(self.normalized.load(Ordering::Relaxed))
    }

    fn set(&self, v: f64) {
        self.normalized
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    unsafe fn read_state(&self, stream: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else {
            return kInvalidArgument;
        };
        let mut buf = [0u8; 4];
        let mut read = 0;
        let res = unsafe { stream.read(buf.as_mut_ptr().cast::<c_void>(), 4, &mut read) };
        if res != kResultOk || read != 4 {
            return kResultFalse;
        }
        self.set(f64::from(f32::from_le_bytes(buf)));
        kResultOk
    }
}

impl Class for Gain {
    type Interfaces = (IComponent, IAudioProcessor, IEditController);
}

impl IPluginBaseTrait for Gain {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Gain {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        // Single-component effect: the host queries IEditController on the component.
        match self.controller_cid {
            Some(cid) if !class_id.is_null() => {
                unsafe { *class_id = cid };
                kResultOk
            }
            _ => kNotImplemented,
        }
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, _dir: BusDirection) -> i32 {
        i32::from(media_type == MediaTypes_::kAudio as MediaType)
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if media_type != MediaTypes_::kAudio as MediaType || index != 0 || bus.is_null() {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = MediaTypes_::kAudio as MediaType;
        bus.direction = dir;
        bus.channelCount = 2;
        copy_wstring(
            if dir == BusDirections_::kInput as BusDirection {
                "Input"
            } else {
                "Output"
            },
            &mut bus.name,
        );
        bus.busType = BusTypes_::kMain as BusType;
        #[allow(clippy::unnecessary_cast)] // enum constant width differs per target
        let flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        bus.flags = flags;
        kResultOk
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        unsafe { self.read_state(state) }
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = (self.get() as f32).to_le_bytes();
        let mut written = 0;
        let res = unsafe { stream.write(bytes.as_mut_ptr().cast::<c_void>(), 4, &mut written) };
        if res == kResultOk && written == 4 {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IAudioProcessorTrait for Gain {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins != 1 || num_outs != 1 || inputs.is_null() || outputs.is_null() {
            return kResultFalse;
        }
        let stereo = unsafe { *inputs == SpeakerArr::kStereo && *outputs == SpeakerArr::kStereo };
        if stereo { kResultTrue } else { kResultFalse }
    }

    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 || arr.is_null() {
            return kInvalidArgument;
        }
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        self.set_processing_result
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if data.is_null() {
            return kInvalidArgument;
        }
        let data = unsafe { &*data };
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for i in 0..unsafe { changes.getParameterCount() } {
                let Some(queue) = (unsafe { ComRef::from_raw(changes.getParameterData(i)) }) else {
                    continue;
                };
                let points = unsafe { queue.getPointCount() };
                if unsafe { queue.getParameterId() } != 0 || points < 1 {
                    continue;
                }
                let (mut offset, mut value) = (0, 0.0);
                // The last point wins: no per-sample ramps in a test fixture.
                if unsafe { queue.getPoint(points - 1, &mut offset, &mut value) } == kResultTrue {
                    self.set(value);
                }
            }
        }

        if data.numInputs < 1
            || data.numOutputs < 1
            || data.inputs.is_null()
            || data.outputs.is_null()
        {
            return kResultOk;
        }
        let (input, output) = unsafe { (&*data.inputs, &*data.outputs) };
        let channels = input.numChannels.min(output.numChannels).max(0) as usize;
        let (ins, outs) = unsafe {
            (
                input.__field0.channelBuffers32,
                output.__field0.channelBuffers32,
            )
        };
        if ins.is_null() || outs.is_null() {
            return kResultOk;
        }
        let g = gain(self.get());
        for c in 0..channels {
            // Raw pointers: hosts may process in place, where &/&mut slices would alias.
            let (src, dst) = unsafe { (*ins.add(c), *outs.add(c)) };
            if src.is_null() || dst.is_null() {
                continue;
            }
            for n in 0..data.numSamples.max(0) as usize {
                unsafe { *dst.add(n) = *src.add(n) * g };
            }
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IEditControllerTrait for Gain {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        unsafe { self.read_state(state) }
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        1
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.id = 0;
        copy_wstring("Gain", &mut info.title);
        copy_wstring("Gain", &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = DEFAULT_NORMALIZED;
        info.unitId = 0;
        #[allow(clippy::unnecessary_cast)] // enum constant width differs per target
        let flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
        info.flags = flags;
        kResultOk
    }

    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        if id != 0 || string.is_null() {
            return kInvalidArgument;
        }
        copy_wstring(&format!("{:.2}", gain(value)), unsafe { &mut *string });
        kResultOk
    }

    unsafe fn getParamValueByString(&self, _id: u32, _s: *mut TChar, _v: *mut f64) -> tresult {
        kNotImplemented
    }

    unsafe fn normalizedParamToPlain(&self, _id: u32, normalized: f64) -> f64 {
        f64::from(gain(normalized))
    }

    unsafe fn plainParamToNormalized(&self, _id: u32, plain: f64) -> f64 {
        (plain / 2.0).clamp(0.0, 1.0)
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        if id == 0 { self.get() } else { 0.0 }
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if id != 0 {
            return kInvalidArgument;
        }
        self.set(value);
        kResultOk
    }

    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }

    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        ptr::null_mut()
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        if info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring("", &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        1
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(PLUGIN_NAME, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        let gain = match unsafe { *cid.cast::<TUID>() } {
            CID => Gain::new(None),
            NOT_IMPL_CID => Gain {
                set_processing_result: kNotImplemented,
                ..Gain::new(None)
            },
            SPLIT_CID => Gain::new(Some(SPLIT_CONTROLLER_CID)),
            SPLIT_CONTROLLER_CID => Gain::new(None),
            _ => return kInvalidArgument,
        };
        let Some(instance) = ComWrapper::new(gain).to_com_ptr::<FUnknown>() else {
            return kInternalError;
        };
        let raw = instance.as_ptr();
        // queryInterface adds the reference handed to the caller; ours drops with `instance`.
        unsafe { ((*(*raw).vtbl).queryInterface)(raw, iid.cast_mut().cast::<TUID>(), obj) }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(PLUGIN_NAME, &mut info.name);
        info.classFlags = 0;
        copy_cstring("Fx", &mut info.subCategories);
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring("1.0.0", &mut info.version);
        copy_cstring("VST 3.7", &mut info.sdkVersion);
        kResultOk
    }
}

#[unsafe(no_mangle)]
extern "C" fn GetPluginFactory() -> *mut IPluginFactory {
    match ComWrapper::new(Factory).to_com_ptr::<IPluginFactory>() {
        Some(factory) => factory.into_raw(),
        None => ptr::null_mut(),
    }
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "C" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "C" fn bundleExit() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "C" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "C" fn ModuleExit() -> bool {
    true
}
