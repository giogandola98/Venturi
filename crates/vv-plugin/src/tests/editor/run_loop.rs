use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, ITimerHandler, ITimerHandlerTrait,
};
use vst3::{Class, ComPtr, ComWrapper};

use super::*;

/// Unregisters itself from its destructor, as a plugin releasing its own state may.
struct Timers {
    run_loop: Rc<RunLoop>,
    me: Rc<Cell<usize>>,
}

impl Class for Timers {
    type Interfaces = (ITimerHandler,);
}

impl ITimerHandlerTrait for Timers {
    unsafe fn onTimer(&self) {}
}

impl Drop for Timers {
    fn drop(&mut self) {
        self.run_loop
            .remove_timer(self.me.get() as *mut ITimerHandler);
    }
}

struct Events {
    run_loop: Rc<RunLoop>,
    me: Rc<Cell<usize>>,
}

impl Class for Events {
    type Interfaces = (IEventHandler,);
}

impl IEventHandlerTrait for Events {
    unsafe fn onFDIsSet(&self, _: FileDescriptor) {}
}

impl Drop for Events {
    fn drop(&mut self) {
        self.run_loop
            .remove_handler(self.me.get() as *mut IEventHandler);
    }
}

fn timer(run_loop: &Rc<RunLoop>) -> ComPtr<ITimerHandler> {
    let me = Rc::new(Cell::new(0));
    let ptr = ComWrapper::new(Timers {
        run_loop: run_loop.clone(),
        me: me.clone(),
    })
    .to_com_ptr::<ITimerHandler>()
    .unwrap();
    me.set(ptr.as_ptr() as usize);
    ptr
}

fn event(run_loop: &Rc<RunLoop>) -> ComPtr<IEventHandler> {
    let me = Rc::new(Cell::new(0));
    let ptr = ComWrapper::new(Events {
        run_loop: run_loop.clone(),
        me: me.clone(),
    })
    .to_com_ptr::<IEventHandler>()
    .unwrap();
    me.set(ptr.as_ptr() as usize);
    ptr
}

const TICK: Duration = Duration::from_millis(5);

#[test]
fn clear_survives_handlers_that_unregister_in_their_destructor() {
    let rl = Rc::new(RunLoop::default());
    // Only the host list owns them once the locals are gone.
    assert_eq!(rl.add_timer(timer(&rl), TICK), kResultOk);
    assert_eq!(rl.add_handler(event(&rl), 3), kResultOk);
    rl.clear();
    assert!(rl.next_wake().is_none());
    assert!(rl.handlers().is_empty());
    assert!(
        rl.take_due(Instant::now() + Duration::from_secs(1))
            .is_empty()
    );
}

#[test]
fn unregister_survives_handlers_that_unregister_in_their_destructor() {
    let rl = Rc::new(RunLoop::default());
    let (gone, kept) = (timer(&rl), timer(&rl));
    let (gone_ptr, kept_ptr) = (gone.as_ptr(), kept.as_ptr());
    rl.add_timer(gone, TICK);
    rl.add_timer(kept, TICK);
    assert_eq!(rl.remove_timer(gone_ptr), kResultOk);
    let due = rl.take_due(Instant::now() + Duration::from_secs(1));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].as_ptr(), kept_ptr);
    drop(due);

    let (gone, kept) = (event(&rl), event(&rl));
    let (gone_ptr, kept_ptr) = (gone.as_ptr(), kept.as_ptr());
    rl.add_handler(gone, 1);
    rl.add_handler(kept, 2);
    assert_eq!(rl.remove_handler(gone_ptr), kResultOk);
    let left = rl.handlers();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].0.as_ptr(), kept_ptr);
    drop(left);
    rl.clear();
}

#[test]
fn a_reentrant_mutation_fails_instead_of_panicking() {
    let rl = Rc::new(RunLoop::default());
    let _held = rl.timers.borrow_mut();
    assert_eq!(rl.add_timer(timer(&rl), TICK), kResultFalse);
    assert_eq!(rl.remove_timer(std::ptr::null_mut()), kResultFalse);
}

#[test]
fn a_registered_fd_does_not_ask_for_polling() {
    let rl = Rc::new(RunLoop::default());
    rl.add_handler(event(&rl), 3);
    assert!(rl.next_wake().is_none());
    rl.add_timer(timer(&rl), TICK);
    assert!(rl.next_wake().is_some_and(|d| d <= TICK));
    rl.clear();
}

#[test]
fn a_registered_fd_wakes_the_app_until_the_pump_rearms() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let rl = Rc::new(RunLoop::new(
        Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }),
        None,
    ));
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    rl.add_handler(event(&rl), fds[0]);
    assert_eq!(unsafe { libc::write(fds[1], [1u8].as_ptr().cast(), 1) }, 1);
    let end = Instant::now() + Duration::from_secs(5);
    while count.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    rl.rearm();
    while count.load(Ordering::SeqCst) < 2 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    rl.clear();
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}
