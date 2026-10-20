use vst3::{ComPtr, ComRef};

use super::*;

#[test]
fn stream_seek_past_end_reads_nothing_and_negative_is_refused() {
    use vst3::Steinberg::{IBStream, IBStreamTrait};
    let stream = MemStream::new(vec![1, 2, 3]);
    let s = stream.as_com_ref::<IBStream>().unwrap();
    let mut pos = 0;
    let mut buf = [0u8; 4];
    let mut read = -1;
    // SAFETY: valid out-pointers on a live stream.
    unsafe {
        assert_eq!(s.seek(10, 0, &mut pos), kResultOk);
        assert_eq!(pos, 10);
        assert_eq!(s.read(buf.as_mut_ptr().cast(), 4, &mut read), kResultOk);
        assert_eq!(read, 0);
        assert_ne!(s.seek(-1, 0, &mut pos), kResultOk);
        assert_eq!(s.seek(0, 1, &mut pos), kResultOk);
        assert_eq!(pos, 10);
    }
}

#[test]
fn stream_seek_forward_then_write_zero_fills_the_gap() {
    use vst3::Steinberg::{IBStream, IBStreamTrait};
    let stream = MemStream::new(Vec::new());
    let s = stream.as_com_ref::<IBStream>().unwrap();
    let body = [7u8, 8, 9];
    let mut written = 0;
    // SAFETY: valid buffers and out-pointers on a live stream.
    unsafe {
        assert_eq!(s.seek(4, 1, std::ptr::null_mut()), kResultOk);
        assert_eq!(
            s.write(body.as_ptr().cast_mut().cast(), 3, &mut written),
            kResultOk
        );
        assert_eq!(written, 3);
    }
    assert_eq!(stream.take(), [0, 0, 0, 0, 7, 8, 9]);
}

#[test]
fn stream_header_reserved_by_seeking_forward_keeps_the_body() {
    use vst3::Steinberg::{IBStream, IBStreamTrait};
    let stream = MemStream::new(Vec::new());
    let s = stream.as_com_ref::<IBStream>().unwrap();
    let body = [1u8, 2, 3, 4, 5];
    let size = (body.len() as u32).to_le_bytes();
    // SAFETY: valid buffers and out-pointers on a live stream.
    unsafe {
        assert_eq!(s.seek(4, 1, std::ptr::null_mut()), kResultOk);
        assert_eq!(
            s.write(body.as_ptr().cast_mut().cast(), 5, std::ptr::null_mut()),
            kResultOk
        );
        assert_eq!(s.seek(0, 0, std::ptr::null_mut()), kResultOk);
        assert_eq!(
            s.write(size.as_ptr().cast_mut().cast(), 4, std::ptr::null_mut()),
            kResultOk
        );
    }
    assert_eq!(stream.take(), [5, 0, 0, 0, 1, 2, 3, 4, 5]);
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
        let text: Vec<u16> = "word\0".encode_utf16().collect();
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
