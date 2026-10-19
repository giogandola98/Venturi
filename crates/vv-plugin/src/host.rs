//! Host-side COM objects handed to plugins. Their methods are called from
//! plugin code, so none of them may panic.
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use vst3::Steinberg::Vst::{
    IComponentHandler, IComponentHandlerTrait, IHostApplication, IHostApplicationTrait,
    IParamValueQueue, IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, ParamID,
    ParamValue, RestartFlags_, String128,
};
use vst3::Steinberg::{
    IBStream, IBStream_::IStreamSeekMode_, IBStreamTrait, TUID, int32, int64, kInvalidArgument,
    kNotImplemented, kResultFalse, kResultOk, tresult,
};
use vst3::{Class, ComWrapper};

/// Distinct parameters one block can carry; the rest are dropped.
pub(crate) const MAX_PARAMS: usize = 64;

pub(crate) struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: non-null out-parameter of the documented size.
        let name = unsafe { &mut *name };
        name.fill(0);
        for (d, s) in name.iter_mut().zip("Venturi".encode_utf16()) {
            *d = s;
        }
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        _cid: *mut TUID,
        _iid: *mut TUID,
        _obj: *mut *mut c_void,
    ) -> tresult {
        kNotImplemented
    }
}

/// What the plugin's controller reports back, shared with the instance.
pub(crate) struct Shared {
    /// Latest value per parameter; coalesced so it never outgrows its capacity.
    pub params: Mutex<Vec<(ParamID, ParamValue)>>,
    pub dirty: AtomicBool,
    pub latency_changed: AtomicBool,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            params: Mutex::new(Vec::with_capacity(MAX_PARAMS)),
            dirty: AtomicBool::new(false),
            latency_changed: AtomicBool::new(false),
        }
    }
}

pub(crate) struct ComponentHandler(pub std::sync::Arc<Shared>);

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        let mut params = self.0.params.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = params.iter_mut().find(|p| p.0 == id) {
            slot.1 = value;
        } else if params.len() < MAX_PARAMS {
            params.push((id, value));
        }
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        self.0.dirty.store(true, Ordering::Release);
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        if flags & RestartFlags_::kLatencyChanged != 0 {
            self.0.latency_changed.store(true, Ordering::Release);
        }
        kResultOk
    }
}

/// A growable byte stream the plugin reads and writes its state through.
pub(crate) struct MemStream {
    state: Mutex<(Vec<u8>, usize)>,
}

impl MemStream {
    pub fn new(bytes: Vec<u8>) -> ComWrapper<Self> {
        ComWrapper::new(Self {
            state: Mutex::new((bytes, 0)),
        })
    }

    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()).0)
    }
}

impl Class for MemStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: int32, read: *mut int32) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (data, pos) = &mut *state;
        let n = (num_bytes as usize).min(data.len().saturating_sub(*pos));
        // SAFETY: the caller's buffer holds num_bytes; n never exceeds that.
        unsafe { std::ptr::copy_nonoverlapping(data[*pos..].as_ptr(), buffer.cast::<u8>(), n) };
        *pos += n;
        if !read.is_null() {
            unsafe { *read = n as int32 };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, num_bytes: int32, written: *mut int32) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (data, pos) = &mut *state;
        let n = num_bytes as usize;
        // SAFETY: the caller's buffer holds num_bytes.
        let src = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), n) };
        if data.len() < *pos + n {
            data.resize(*pos + n, 0);
        }
        data[*pos..*pos + n].copy_from_slice(src);
        *pos += n;
        if !written.is_null() {
            unsafe { *written = num_bytes };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (data, cursor) = &mut *state;
        #[allow(clippy::unnecessary_cast)] // u32 or i32 depending on the target
        let base = match mode {
            m if m == IStreamSeekMode_::kIBSeekSet as i32 => 0,
            m if m == IStreamSeekMode_::kIBSeekCur as i32 => *cursor as int64,
            m if m == IStreamSeekMode_::kIBSeekEnd as i32 => data.len() as int64,
            _ => return kInvalidArgument,
        };
        match base.checked_add(pos) {
            Some(target) if target >= 0 => *cursor = target as usize,
            _ => return kResultFalse,
        }
        if !result.is_null() {
            unsafe { *result = *cursor as int64 };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        let cursor = self.state.lock().unwrap_or_else(|e| e.into_inner()).1;
        unsafe { *pos = cursor as int64 };
        kResultOk
    }
}

/// One parameter with its single point, at sample offset 0.
struct ParamQueue {
    id: Cell<ParamID>,
    value: Cell<ParamValue>,
    // The output side takes points from the plugin and forgets them.
    sink: bool,
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> int32 {
        i32::from(!self.sink)
    }

    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        if index != 0 || self.sink || offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        unsafe {
            *offset = 0;
            *value = self.value.get();
        }
        kResultOk
    }

    unsafe fn addPoint(&self, _offset: int32, _value: ParamValue, index: *mut int32) -> tresult {
        if !self.sink {
            return kResultFalse;
        }
        if !index.is_null() {
            unsafe { *index = 0 };
        }
        kResultOk
    }
}

/// Fixed-capacity parameter changes for one block, reused without allocating.
pub(crate) struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    count: Cell<usize>,
    // Some plugins write output changes unconditionally.
    sink: Option<ComWrapper<ParamQueue>>,
}

// SAFETY: the cells are only touched by the thread that holds the instance's
// `busy` flag, or by the plugin during its `process` call on that thread.
unsafe impl Send for ParamChanges {}
unsafe impl Sync for ParamChanges {}

impl ParamChanges {
    fn make(sink: bool) -> ComWrapper<Self> {
        let queue = |sink| {
            ComWrapper::new(ParamQueue {
                id: Cell::new(0),
                value: Cell::new(0.0),
                sink,
            })
        };
        ComWrapper::new(Self {
            queues: if sink {
                Vec::new()
            } else {
                (0..MAX_PARAMS).map(|_| queue(false)).collect()
            },
            count: Cell::new(0),
            sink: sink.then(|| queue(true)),
        })
    }

    pub fn input() -> ComWrapper<Self> {
        Self::make(false)
    }

    pub fn output_sink() -> ComWrapper<Self> {
        Self::make(true)
    }

    pub fn clear(&self) {
        self.count.set(0);
    }

    pub fn push(&self, id: ParamID, value: ParamValue) {
        let n = self.count.get();
        if let Some(q) = self.queues.get(n) {
            q.id.set(id);
            q.value.set(value);
            self.count.set(n + 1);
        }
    }
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.count.get() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        let queue = usize::try_from(index)
            .ok()
            .filter(|&i| i < self.count.get())
            .and_then(|i| self.queues.get(i));
        queue
            .and_then(|q| q.as_com_ref::<IParamValueQueue>())
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }

    unsafe fn addParameterData(
        &self,
        _id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        let Some(sink) = &self.sink else {
            return std::ptr::null_mut();
        };
        if !index.is_null() {
            unsafe { *index = 0 };
        }
        sink.as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}
