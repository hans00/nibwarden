//! Sleep: IOKit's system power notifications (`IORegisterForSystemPower`), delivered on a
//! thread of our own through its run loop. Screen lock: that thread polls the GUI
//! session's `CGSSessionScreenIsLocked` once a second. (The `com.apple.screenIsLocked`
//! distributed notification would be instant, but where its callbacks run, a thread's
//! run loop or the main one, is not documented, and a daemon's main thread has no run
//! loop; a poll cannot go quiet.)

use std::{
    ffi::c_void,
    io,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use core_foundation::runloop::{CFRunLoop, CFRunLoopRunResult};
use core_foundation_sys::runloop::{
    CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopSourceRef, kCFRunLoopCommonModes,
    kCFRunLoopDefaultMode,
};
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use super::SessionEvent;
use crate::gui_session;

/// `mach_port_t` handles of IOKit.
type IoConnect = u32;
type IoObject = u32;
type NotificationPort = *mut c_void;
/// `IOServiceInterestCallback`.
type PowerCallback = extern "C" fn(
    refcon: *mut c_void,
    service: IoObject,
    message_type: u32,
    message_argument: *mut c_void,
);

/// `kIOMessageCanSystemSleep`, `kIOMessageSystemWillSleep` (`IOMessage.h`).
const CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
const SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;

/// How long sleep is held back after the event is sent, so that the daemon's lock goes
/// through before the machine stops (it may wait 30 s).
const GRACE: Duration = Duration::from_millis(300);
const POLL: Duration = Duration::from_secs(1);

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        port: *mut NotificationPort,
        callback: PowerCallback,
        notifier: *mut IoObject,
    ) -> IoConnect;
    fn IODeregisterForSystemPower(notifier: *mut IoObject) -> i32;
    fn IOAllowPowerChange(kernel_port: IoConnect, notification_id: isize) -> i32;
    fn IOServiceClose(connect: IoConnect) -> i32;
    fn IONotificationPortGetRunLoopSource(port: NotificationPort) -> CFRunLoopSourceRef;
    fn IONotificationPortDestroy(port: NotificationPort);
}

struct PowerState {
    events: UnboundedSender<SessionEvent>,
    /// The connection to the power manager, which the callback answers on.
    root: AtomicU32,
}

extern "C" fn power_callback(
    refcon: *mut c_void,
    _service: IoObject,
    message_type: u32,
    message_argument: *mut c_void,
) {
    // SAFETY: `refcon` is the PowerState that outlives the registration (see `run`).
    let state = unsafe { &*refcon.cast::<PowerState>() };
    match message_type {
        CAN_SYSTEM_SLEEP => allow(state, message_argument),
        SYSTEM_WILL_SLEEP => {
            let _ = state.events.send(SessionEvent::Sleeping);
            std::thread::sleep(GRACE);
            allow(state, message_argument);
        }
        _ => {}
    }
}

/// Tells the power manager that this client does not hold up the change.
fn allow(state: &PowerState, id: *mut c_void) {
    // SAFETY: `root` is the connection returned by IORegisterForSystemPower, and the
    // notification id is the callback's argument.
    unsafe { IOAllowPowerChange(state.root.load(Ordering::Relaxed), id as isize) };
}

pub async fn watch(events: UnboundedSender<SessionEvent>) -> io::Result<()> {
    let (ready_tx, ready_rx) = oneshot::channel();
    let thread_events = events.clone();
    std::thread::Builder::new()
        .name("session-events".into())
        .spawn(move || run(thread_events, ready_tx))?;
    ready_rx
        .await
        .map_err(|_| io::Error::other("the session event thread ended"))??;
    // The thread notices at its next tick that nobody listens any more.
    events.closed().await;
    Ok(())
}

fn run(events: UnboundedSender<SessionEvent>, ready: oneshot::Sender<io::Result<()>>) {
    let state = Box::into_raw(Box::new(PowerState {
        events: events.clone(),
        root: AtomicU32::new(0),
    }));
    let mut port: NotificationPort = std::ptr::null_mut();
    let mut notifier: IoObject = 0;
    // SAFETY: `state` stays valid until it is freed below, after the notifier is
    // deregistered; the out-parameters are valid for writes.
    let root =
        unsafe { IORegisterForSystemPower(state.cast(), &mut port, power_callback, &mut notifier) };
    if root == 0 {
        // SAFETY: nothing registered the state; we are its only owner.
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err(io::Error::other("IORegisterForSystemPower failed")));
        return;
    }
    // SAFETY: the state is alive; the notification port's run loop source is added to
    // this thread's run loop, which is the one driven below.
    unsafe {
        (*state).root.store(root, Ordering::Relaxed);
        CFRunLoopAddSource(
            CFRunLoopGetCurrent(),
            IONotificationPortGetRunLoopSource(port),
            kCFRunLoopCommonModes,
        );
    }
    let _ = ready.send(Ok(()));

    let mut was_locked = gui_session::screen_locked();
    while !events.is_closed() {
        // SAFETY: kCFRunLoopDefaultMode is a constant CFString.
        let result = CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, POLL, false);
        // A run loop without sources returns at once; do not spin.
        if matches!(result, CFRunLoopRunResult::Finished) {
            std::thread::sleep(POLL);
        }
        let locked = gui_session::screen_locked();
        if locked && !was_locked && events.send(SessionEvent::ScreenLocked).is_err() {
            break;
        }
        was_locked = locked;
    }

    // SAFETY: undoes the registration; afterwards no callback can run, so the state can
    // be freed.
    unsafe {
        IODeregisterForSystemPower(&mut notifier);
        IOServiceClose(root);
        IONotificationPortDestroy(port);
        drop(Box::from_raw(state));
    }
}
