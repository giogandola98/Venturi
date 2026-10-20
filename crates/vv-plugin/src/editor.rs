//! A plugin's own GUI in a native window. UI thread only: the plugin's view
//! and the window system both require it.
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::Arc;

use vst3::Steinberg::Vst::{IEditControllerTrait, ViewType};
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, ViewRect, kInternalError,
    kInvalidArgument, kResultFalse, kResultOk, kResultTrue, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};

use crate::{HostedPlugin, Waker};

#[cfg(target_os = "macos")]
#[path = "editor/macos.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "editor/x11.rs"]
mod platform;

const METER_POLL: std::time::Duration = std::time::Duration::from_millis(33);

type Resizer = Rc<dyn Fn(i32, i32)>;

/// What the plugin calls back on: resizes the window, then tells the view.
struct Frame {
    resize: RefCell<Option<Resizer>>,
    resizing: Cell<bool>,
    #[cfg(target_os = "linux")]
    run_loop: platform::RunLoop,
}

impl Class for Frame {
    #[cfg(target_os = "linux")]
    type Interfaces = (IPlugFrame, vst3::Steinberg::Linux::IRunLoop);
    #[cfg(not(target_os = "linux"))]
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if view.is_null() || new_size.is_null() {
            return kInvalidArgument;
        }
        // A resize caused by our own onSize must not start another.
        if self.resizing.replace(true) {
            return kResultFalse;
        }
        let res = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: non-null, and the plugin owns the rect for this call.
            let rect = unsafe { &mut *new_size };
            let resize = self.resize.borrow().clone();
            if let Some(resize) = resize {
                resize(rect.right - rect.left, rect.bottom - rect.top);
            }
            // SAFETY: the plugin passes its own live view.
            if let Some(view) = unsafe { ComRef::from_raw(view) } {
                unsafe { view.onSize(rect) };
            }
            kResultOk
        }))
        .unwrap_or(kInternalError);
        self.resizing.set(false);
        res
    }
}

pub struct Editor {
    // Order matters: the view goes before the window it lives in, and the
    // instance (the plugin's code) last.
    view: ComPtr<IPlugView>,
    frame: ComWrapper<Frame>,
    window: platform::Window,
    resize: Resizer,
    instance: Arc<dyn HostedPlugin>,
}

fn size_of(rect: &ViewRect) -> (i32, i32) {
    (rect.right - rect.left, rect.bottom - rect.top)
}

impl Editor {
    /// Err if the plugin has no editor or the platform view type is unsupported.
    /// `waker` is called, from any thread, when the editor needs `idle` or the
    /// app should look at the instance (an edit ended, the window changed).
    pub fn open(
        instance: &Arc<dyn HostedPlugin>,
        title: &str,
        waker: Waker,
    ) -> Result<Self, String> {
        platform::check_thread()?;
        let controller = instance
            .as_vst3()
            .and_then(|i| i.controller())
            .ok_or("the plugin has no controller")?;
        // SAFETY: a live controller; the view comes back with one reference for us.
        let view = unsafe { controller.createView(ViewType::kEditor) };
        // SAFETY: owns the reference createView returned.
        let view = unsafe { ComPtr::from_raw(view) }.ok_or("the plugin has no editor")?;
        // SAFETY: calls on the live view.
        let (size, resizable) = unsafe {
            if view.isPlatformTypeSupported(platform::VIEW_TYPE) != kResultTrue {
                return Err("the plugin editor does not support this platform".into());
            }
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if view.getSize(&mut rect) != kResultOk {
                return Err("the plugin editor reports no size".into());
            }
            (size_of(&rect), view.canResize() == kResultTrue)
        };
        let window = platform::Window::new(title, size, resizable, waker.clone())?;
        let resize = window.resizer();
        let frame = ComWrapper::new(Frame {
            resize: RefCell::new(Some(resize.clone())),
            resizing: Cell::new(false),
            #[cfg(target_os = "linux")]
            run_loop: Default::default(),
        });
        let frame_ptr = frame.to_com_ptr::<IPlugFrame>().ok_or("plugin frame")?;
        // SAFETY: the frame outlives the view's use of it (cleared in Drop).
        unsafe { view.setFrame(frame_ptr.as_ptr()) };
        // SAFETY: the parent is a live native view/window of the platform's type.
        let attached = unsafe { view.attached(window.parent(), platform::VIEW_TYPE) };
        if attached != kResultOk {
            // SAFETY: detaches the frame set above; the view was never attached.
            unsafe { view.setFrame(std::ptr::null_mut()) };
            frame.resize.take();
            return Err(format!("the plugin editor did not attach ({attached})"));
        }
        if let Some(vst3) = instance.as_vst3() {
            vst3.set_waker(Some(waker));
        }
        Ok(Self {
            view,
            frame,
            window,
            resize,
            instance: instance.clone(),
        })
    }

    pub fn instance(&self) -> &Arc<dyn HostedPlugin> {
        &self.instance
    }

    /// Pumps the window and, on Linux, the plugin's registered fds and timers.
    pub fn idle(&mut self) {
        if let Some(vst3) = self.instance.as_vst3() {
            vst3.forward_outputs();
        }
        #[cfg(target_os = "linux")]
        self.frame.run_loop.pump();
        if let Some((width, height)) = self.window.idle() {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            // SAFETY: the view is live until Drop.
            unsafe {
                self.view.checkSizeConstraint(&mut rect);
                if size_of(&rect) != (width, height) {
                    (self.resize)(rect.right - rect.left, rect.bottom - rect.top);
                }
                self.view.onSize(&mut rect);
            }
        }
    }

    /// When `idle` is next needed besides the waker's calls; `None` to sleep.
    pub fn next_wake(&self) -> Option<std::time::Duration> {
        // Playback already repaints; this only keeps a meter moving when a plugin's output changes
        // while nothing else wakes the app.
        let meters = self
            .instance
            .as_vst3()
            .is_some_and(|v| v.outputs_fresh())
            .then_some(METER_POLL);
        #[cfg(target_os = "linux")]
        return match (meters, self.frame.run_loop.next_wake()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        #[cfg(not(target_os = "linux"))]
        meters
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    /// Shows the window again, in front and focused, even if covered or minimized.
    pub fn raise(&self) {
        self.window.raise();
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        // SAFETY: the view was attached in `open`; the frame is released first
        // so the plugin cannot call back into a closing window.
        unsafe {
            self.view.removed();
            self.view.setFrame(std::ptr::null_mut());
        }
        self.frame.resize.take();
        #[cfg(target_os = "linux")]
        self.frame.run_loop.clear();
        if let Some(vst3) = self.instance.as_vst3() {
            vst3.set_waker(None);
        }
    }
}

#[cfg(test)]
#[path = "tests/editor.rs"]
mod tests;
