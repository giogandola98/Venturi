//! The editor's top-level X11 window (the plugin embeds into it with XEmbed)
//! and the run loop plugins expect from a Linux host.
use std::cell::{Cell, RefCell};
use std::ffi::{c_int, c_void};
use std::rc::Rc;
use std::time::{Duration, Instant};

use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, IRunLoopTrait, ITimerHandler,
    ITimerHandlerTrait, TimerInterval,
};
use vst3::Steinberg::{
    FIDString, kInvalidArgument, kPlatformTypeX11EmbedWindowID, kResultOk, tresult,
};
use vst3::{ComPtr, ComRef};
use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT};

use super::{Frame, Resizer};
use crate::Waker;

pub(super) fn check_thread() -> Result<(), String> {
    Ok(())
}

pub(super) const VIEW_TYPE: FIDString = kPlatformTypeX11EmbedWindowID;

pub(super) struct Window {
    conn: Rc<RustConnection>,
    id: u32,
    delete: u32,
    resizable: bool,
    size: Rc<Cell<(i32, i32)>>,
    closed: bool,
}

fn clamp(v: i32) -> u16 {
    v.clamp(1, i32::from(u16::MAX)) as u16
}

// Without hints a window manager would let the user resize a fixed-size editor.
fn pin_size(conn: &RustConnection, id: u32, (width, height): (i32, i32)) {
    let mut hints = WmSizeHints::new();
    hints.min_size = Some((width, height));
    hints.max_size = Some((width, height));
    let _ = hints.set_normal_hints(conn, id);
}

impl Window {
    pub fn new(
        title: &str,
        size: (i32, i32),
        resizable: bool,
        _waker: Waker,
    ) -> Result<Self, String> {
        let fail = |e: &dyn std::fmt::Display| format!("X11: {e}");
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| fail(&e))?;
        let screen = &conn.setup().roots[screen_num];
        let id = conn.generate_id().map_err(|e| fail(&e))?;
        let aux = CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(EventMask::STRUCTURE_NOTIFY);
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            id,
            screen.root,
            0,
            0,
            clamp(size.0),
            clamp(size.1),
            0,
            WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &aux,
        )
        .map_err(|e| fail(&e))?;
        let atom = |name: &str| -> Result<u32, String> {
            let cookie = conn
                .intern_atom(false, name.as_bytes())
                .map_err(|e| fail(&e))?;
            Ok(cookie.reply().map_err(|e| fail(&e))?.atom)
        };
        let (protocols, delete) = (atom("WM_PROTOCOLS")?, atom("WM_DELETE_WINDOW")?);
        let (net_name, utf8) = (atom("_NET_WM_NAME")?, atom("UTF8_STRING")?);
        let _ = conn.change_property8(
            PropMode::REPLACE,
            id,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        );
        let _ = conn.change_property8(PropMode::REPLACE, id, net_name, utf8, title.as_bytes());
        let _ = conn.change_property32(PropMode::REPLACE, id, protocols, AtomEnum::ATOM, &[delete]);
        if !resizable {
            pin_size(&conn, id, size);
        }
        conn.map_window(id).map_err(|e| fail(&e))?;
        // The plugin creates its child on another connection: the window must exist by then.
        conn.get_input_focus()
            .map_err(|e| fail(&e))?
            .reply()
            .map_err(|e| fail(&e))?;
        Ok(Self {
            conn: Rc::new(conn),
            id,
            delete,
            resizable,
            size: Rc::new(Cell::new(size)),
            closed: false,
        })
    }

    /// XEmbed takes the window id as the parent pointer.
    pub fn parent(&self) -> *mut c_void {
        self.id as usize as *mut c_void
    }

    pub fn resizer(&self) -> Resizer {
        let (conn, id, size, resizable) = (
            self.conn.clone(),
            self.id,
            self.size.clone(),
            self.resizable,
        );
        Rc::new(move |width, height| {
            size.set((width, height));
            if !resizable {
                pin_size(&conn, id, (width, height));
            }
            let aux = ConfigureWindowAux::new()
                .width(u32::from(clamp(width)))
                .height(u32::from(clamp(height)));
            let _ = conn.configure_window(id, &aux);
            let _ = conn.flush();
        })
    }

    /// The new size when the user resized the window.
    pub fn idle(&mut self) -> Option<(i32, i32)> {
        let mut resized = None;
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(Event::ClientMessage(m)))
                    if m.window == self.id
                        && m.format == 32
                        && m.data.as_data32()[0] == self.delete =>
                {
                    self.closed = true
                }
                Ok(Some(Event::DestroyNotify(e))) if e.window == self.id => self.closed = true,
                Ok(Some(Event::ConfigureNotify(e))) if e.window == self.id => {
                    let now = (i32::from(e.width), i32::from(e.height));
                    if now != self.size.replace(now) {
                        resized = Some(now);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    self.closed = true;
                    break;
                }
            }
        }
        resized
    }

    pub fn is_open(&self) -> bool {
        !self.closed
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.id);
        let _ = self.conn.flush();
    }
}

const FD_POLL: Duration = Duration::from_millis(16);
/// Keeps `Instant` arithmetic far from overflow.
const MAX_TIMER: Duration = Duration::from_secs(3600);

struct Timer {
    handler: ComPtr<ITimerHandler>,
    interval: Duration,
    next: Instant,
}

/// What the plugin asked of the host's event loop; `pump` serves it.
#[derive(Default)]
pub(super) struct RunLoop {
    handlers: RefCell<Vec<(ComPtr<IEventHandler>, c_int)>>,
    timers: RefCell<Vec<Timer>>,
}

impl RunLoop {
    pub fn clear(&self) {
        self.handlers.borrow_mut().clear();
        self.timers.borrow_mut().clear();
    }

    /// When the plugin needs the loop next: nothing is registered, nothing to wake for.
    /// There is no cheap wake on an fd, so those are polled at a GUI-friendly pace.
    pub fn next_wake(&self) -> Option<Duration> {
        if !self.handlers.borrow().is_empty() {
            return Some(FD_POLL);
        }
        let now = Instant::now();
        let timers = self.timers.borrow();
        let next = timers.iter().map(|t| t.next).min()?;
        Some(next.saturating_duration_since(now))
    }

    /// Calls back what is ready or due; the handlers may register more meanwhile.
    pub fn pump(&self) {
        let handlers = self.handlers.borrow().clone();
        let mut polls: Vec<libc::pollfd> = handlers
            .iter()
            .map(|(_, fd)| libc::pollfd {
                fd: *fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `polls` is a valid array of the length passed; a zero timeout never blocks.
        if !polls.is_empty() && unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, 0) } > 0 {
            for (poll, (handler, fd)) in polls.iter().zip(&handlers) {
                let registered = self
                    .handlers
                    .borrow()
                    .iter()
                    .any(|(h, _)| h.as_ptr() == handler.as_ptr());
                if poll.revents != 0 && registered {
                    // SAFETY: a live handler the plugin registered.
                    unsafe { handler.onFDIsSet(*fd) };
                }
            }
        }
        let now = Instant::now();
        let due: Vec<ComPtr<ITimerHandler>> = self
            .timers
            .borrow_mut()
            .iter_mut()
            .filter(|t| t.next <= now)
            .map(|t| {
                t.next = now.checked_add(t.interval).unwrap_or(now);
                t.handler.clone()
            })
            .collect();
        for handler in due {
            let registered = self
                .timers
                .borrow()
                .iter()
                .any(|t| t.handler.as_ptr() == handler.as_ptr());
            if registered {
                // SAFETY: a live handler the plugin registered.
                unsafe { handler.onTimer() };
            }
        }
    }
}

impl IRunLoopTrait for Frame {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut IEventHandler,
        fd: FileDescriptor,
    ) -> tresult {
        // SAFETY: a live handler from the plugin; the clone takes our own reference.
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        self.run_loop
            .handlers
            .borrow_mut()
            .push((handler.to_com_ptr(), fd));
        kResultOk
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        self.run_loop
            .handlers
            .borrow_mut()
            .retain(|(h, _)| h.as_ptr() != handler);
        kResultOk
    }

    unsafe fn registerTimer(
        &self,
        handler: *mut ITimerHandler,
        milliseconds: TimerInterval,
    ) -> tresult {
        // SAFETY: as in registerEventHandler.
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        let interval =
            Duration::from_millis(milliseconds).clamp(Duration::from_millis(1), MAX_TIMER);
        self.run_loop.timers.borrow_mut().push(Timer {
            handler: handler.to_com_ptr(),
            interval,
            next: Instant::now() + interval,
        });
        kResultOk
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        self.run_loop
            .timers
            .borrow_mut()
            .retain(|t| t.handler.as_ptr() != handler);
        kResultOk
    }
}
