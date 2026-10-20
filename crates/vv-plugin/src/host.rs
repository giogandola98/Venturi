//! Host-side COM objects handed to plugins. Their methods are called from
//! plugin code, so none of them may panic.
use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use vst3::Steinberg::Vst::{
    IAttributeList, IAttributeList_iid, IAttributeListTrait, IComponentHandler,
    IComponentHandlerTrait, IHostApplication, IHostApplicationTrait, IMessage, IMessage_iid,
    IMessageTrait, IParamValueQueue, IParamValueQueueTrait, IParameterChanges,
    IParameterChangesTrait, ParamID, ParamValue, RestartFlags_, String128, TChar,
};
use vst3::Steinberg::{
    IBStream, IBStream_::IStreamSeekMode_, IBStreamTrait, TUID, int32, int64, kInternalError,
    kInvalidArgument, kNotImplemented, kResultFalse, kResultOk, tresult,
};
use vst3::{Class, ComWrapper};

type AttrID = *const c_char;

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
        cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: non-null 16-byte id.
        let cid = unsafe { *cid };
        let raw = if cid == IMessage_iid {
            ComWrapper::new(Message::new())
                .to_com_ptr::<IMessage>()
                .map(|p| p.into_raw().cast::<c_void>())
        } else if cid == IAttributeList_iid {
            ComWrapper::new(Attributes::default())
                .to_com_ptr::<IAttributeList>()
                .map(|p| p.into_raw().cast::<c_void>())
        } else {
            return kNotImplemented;
        };
        match raw {
            Some(raw) => {
                unsafe { *obj = raw };
                kResultOk
            }
            None => kInternalError,
        }
    }
}

/// Called from any thread when the UI has something to do.
pub type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;

/// What the plugin's controller reports back, shared with the instance.
pub(crate) struct Shared {
    /// Latest value per parameter, coalesced by id. The UI thread may grow it; the
    /// audio thread takes at most `MAX_PARAMS` per block and leaves the rest queued.
    pub params: Mutex<Vec<(ParamID, ParamValue)>>,
    pub dirty: AtomicBool,
    pub latency_changed: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            params: Mutex::new(Vec::with_capacity(MAX_PARAMS)),
            dirty: AtomicBool::new(false),
            latency_changed: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }

    pub fn set_waker(&self, waker: Option<Waker>) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = waker;
    }

    fn wake(&self) {
        let waker = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(waker) = waker {
            waker();
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
        } else {
            params.push((id, value));
        }
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        self.0.dirty.store(true, Ordering::Release);
        self.0.wake();
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        if flags & RestartFlags_::kLatencyChanged != 0 {
            self.0.latency_changed.store(true, Ordering::Release);
        }
        self.0.wake();
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
        let src = data.get(*pos..).unwrap_or(&[]);
        // SAFETY: the caller's buffer holds num_bytes; n never exceeds that.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), buffer.cast::<u8>(), n) };
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
        let end = pos.saturating_add(n);
        if data.len() < end {
            data.resize(end, 0);
        }
        if let Some(dst) = data.get_mut(*pos..end) {
            dst.copy_from_slice(src);
        }
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
            // Past the end would make a later write allocate whatever the plugin asks.
            Some(target) if (0..=data.len() as int64).contains(&target) => {
                *cursor = target as usize
            }
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

enum Value {
    Int(i64),
    Float(f64),
    Text(Vec<TChar>),
    Binary(Vec<u8>),
}

/// Attributes of a message; plugins only use them from the UI thread.
#[derive(Default)]
struct Attributes(Mutex<Vec<(String, Value)>>);

impl Attributes {
    fn set(&self, id: AttrID, value: Value) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: attribute ids are NUL-terminated strings.
        let id = unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned();
        let mut list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match list.iter_mut().find(|(k, _)| *k == id) {
            Some(slot) => slot.1 = value,
            None => list.push((id, value)),
        }
        kResultOk
    }

    fn get<T>(&self, id: AttrID, read: impl FnOnce(&Value) -> Option<T>) -> Option<T> {
        if id.is_null() {
            return None;
        }
        // SAFETY: attribute ids are NUL-terminated strings.
        let id = unsafe { CStr::from_ptr(id) }.to_string_lossy();
        let list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        list.iter()
            .find(|(k, _)| *k == id)
            .and_then(|(_, v)| read(v))
    }
}

impl Class for Attributes {
    type Interfaces = (IAttributeList,);
}

impl IAttributeListTrait for Attributes {
    unsafe fn setInt(&self, id: AttrID, value: i64) -> tresult {
        self.set(id, Value::Int(value))
    }

    unsafe fn getInt(&self, id: AttrID, value: *mut i64) -> tresult {
        let found = self.get(id, |v| {
            if let Value::Int(i) = v {
                Some(*i)
            } else {
                None
            }
        });
        match found {
            Some(i) if !value.is_null() => {
                unsafe { *value = i };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        self.set(id, Value::Float(value))
    }

    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        let found = self.get(id, |v| {
            if let Value::Float(f) = v {
                Some(*f)
            } else {
                None
            }
        });
        match found {
            Some(f) if !value.is_null() => {
                unsafe { *value = f };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut text = Vec::new();
        // SAFETY: a NUL-terminated UTF-16 string.
        unsafe {
            let mut p = string;
            while *p != 0 {
                text.push(*p);
                p = p.add(1);
            }
        }
        self.set(id, Value::Text(text))
    }

    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: u32) -> tresult {
        let cap = size_in_bytes as usize / std::mem::size_of::<TChar>();
        let found = self.get(id, |v| {
            if let Value::Text(t) = v {
                Some(t.clone())
            } else {
                None
            }
        });
        match found {
            Some(text) if !string.is_null() && cap > 0 => {
                let n = text.len().min(cap - 1);
                // SAFETY: the buffer holds `cap` units; n + 1 <= cap.
                unsafe {
                    std::ptr::copy_nonoverlapping(text.as_ptr(), string, n);
                    *string.add(n) = 0;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size_in_bytes: u32) -> tresult {
        if data.is_null() && size_in_bytes > 0 {
            return kInvalidArgument;
        }
        let bytes = if size_in_bytes == 0 {
            Vec::new()
        } else {
            // SAFETY: the caller's buffer holds size_in_bytes.
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size_in_bytes as usize) }
                .to_vec()
        };
        self.set(id, Value::Binary(bytes))
    }

    unsafe fn getBinary(
        &self,
        id: AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut u32,
    ) -> tresult {
        if data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        // The pointer stays valid until the attribute is set again, as the SDK's does.
        let found = self.get(id, |v| {
            if let Value::Binary(b) = v {
                Some((b.as_ptr(), b.len()))
            } else {
                None
            }
        });
        match found {
            Some((ptr, len)) => {
                unsafe {
                    *data = ptr.cast::<c_void>();
                    *size_in_bytes = len as u32;
                }
                kResultOk
            }
            None => kResultFalse,
        }
    }
}

struct Message {
    id: Mutex<CString>,
    attributes: ComWrapper<Attributes>,
}

impl Message {
    fn new() -> Self {
        Self {
            id: Mutex::new(CString::default()),
            attributes: ComWrapper::new(Attributes::default()),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> *const c_char {
        // Valid until the id is set again.
        self.id.lock().unwrap_or_else(|e| e.into_inner()).as_ptr()
    }

    unsafe fn setMessageID(&self, id: *const c_char) {
        if id.is_null() {
            return;
        }
        // SAFETY: a NUL-terminated string.
        let id = unsafe { CStr::from_ptr(id) }.to_owned();
        *self.id.lock().unwrap_or_else(|e| e.into_inner()) = id;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}
