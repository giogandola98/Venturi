//! The handler and timer lists behind the Linux `IRunLoop`. Platform-neutral so
//! the bookkeeping is testable without a display.
use std::cell::RefCell;
use std::ffi::c_int;
use std::time::{Duration, Instant};

use vst3::ComPtr;
use vst3::Steinberg::Linux::{IEventHandler, ITimerHandler};
use vst3::Steinberg::{kResultFalse, kResultOk, tresult};

const FD_POLL: Duration = Duration::from_millis(16);

struct Timer {
    handler: ComPtr<ITimerHandler>,
    interval: Duration,
    next: Instant,
}

/// What the plugin asked of the host's event loop.
///
/// Dropping a handler may run the plugin's destructor, which can unregister
/// itself: entries are therefore released only after the borrow is gone.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Default)]
pub(super) struct RunLoop {
    handlers: RefCell<Vec<(ComPtr<IEventHandler>, c_int)>>,
    timers: RefCell<Vec<Timer>>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl RunLoop {
    pub fn clear(&self) {
        let handlers = self
            .handlers
            .try_borrow_mut()
            .map(|mut h| std::mem::take(&mut *h));
        let timers = self
            .timers
            .try_borrow_mut()
            .map(|mut t| std::mem::take(&mut *t));
        drop((handlers, timers));
    }

    pub fn add_handler(&self, handler: ComPtr<IEventHandler>, fd: c_int) -> tresult {
        match self.handlers.try_borrow_mut() {
            Ok(mut list) => {
                list.push((handler, fd));
                kResultOk
            }
            Err(_) => kResultFalse,
        }
    }

    pub fn remove_handler(&self, handler: *mut IEventHandler) -> tresult {
        let removed = match self.handlers.try_borrow_mut() {
            Ok(mut list) => {
                let (gone, kept) = std::mem::take(&mut *list)
                    .into_iter()
                    .partition(|(h, _)| h.as_ptr() == handler);
                *list = kept;
                gone
            }
            Err(_) => return kResultFalse,
        };
        drop::<Vec<_>>(removed);
        kResultOk
    }

    pub fn add_timer(&self, handler: ComPtr<ITimerHandler>, interval: Duration) -> tresult {
        match self.timers.try_borrow_mut() {
            Ok(mut list) => {
                list.push(Timer {
                    handler,
                    interval,
                    next: Instant::now() + interval,
                });
                kResultOk
            }
            Err(_) => kResultFalse,
        }
    }

    pub fn remove_timer(&self, handler: *mut ITimerHandler) -> tresult {
        let removed = match self.timers.try_borrow_mut() {
            Ok(mut list) => {
                let (gone, kept) = std::mem::take(&mut *list)
                    .into_iter()
                    .partition(|t| t.handler.as_ptr() == handler);
                *list = kept;
                gone
            }
            Err(_) => return kResultFalse,
        };
        drop::<Vec<_>>(removed);
        kResultOk
    }

    /// When the plugin needs the loop next: nothing is registered, nothing to wake for.
    /// There is no cheap wake on an fd, so those are polled at a GUI-friendly pace.
    pub fn next_wake(&self) -> Option<Duration> {
        if !self.handlers.try_borrow().ok()?.is_empty() {
            return Some(FD_POLL);
        }
        let now = Instant::now();
        let timers = self.timers.try_borrow().ok()?;
        let next = timers.iter().map(|t| t.next).min()?;
        Some(next.saturating_duration_since(now))
    }

    /// Clones, so the callbacks run with no borrow held.
    pub fn handlers(&self) -> Vec<(ComPtr<IEventHandler>, c_int)> {
        self.handlers
            .try_borrow()
            .map(|h| h.clone())
            .unwrap_or_default()
    }

    pub fn has_handler(&self, handler: &ComPtr<IEventHandler>) -> bool {
        self.handlers
            .try_borrow()
            .is_ok_and(|h| h.iter().any(|(h, _)| h.as_ptr() == handler.as_ptr()))
    }

    /// The timers due at `now`, rescheduled.
    pub fn take_due(&self, now: Instant) -> Vec<ComPtr<ITimerHandler>> {
        let Ok(mut timers) = self.timers.try_borrow_mut() else {
            return Vec::new();
        };
        timers
            .iter_mut()
            .filter(|t| t.next <= now)
            .map(|t| {
                t.next = now.checked_add(t.interval).unwrap_or(now);
                t.handler.clone()
            })
            .collect()
    }

    pub fn has_timer(&self, handler: &ComPtr<ITimerHandler>) -> bool {
        self.timers
            .try_borrow()
            .is_ok_and(|t| t.iter().any(|t| t.handler.as_ptr() == handler.as_ptr()))
    }
}

#[cfg(test)]
#[path = "../tests/editor/run_loop.rs"]
mod tests;
