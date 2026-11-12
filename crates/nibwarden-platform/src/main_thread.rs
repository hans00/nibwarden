//! The loop of the thread that shows the tray icon (phase2.md P2-M8). macOS draws status
//! items only from the main thread, inside AppKit's event loop, and Windows delivers the
//! icon's clicks to the message queue of the thread that made it; so the tray process's
//! main thread waits here, not on a plain channel. Linux needs neither and just waits.

use std::{sync::Mutex, time::Duration};

type Work = Box<dyn FnOnce() + Send>;

static POSTED: Mutex<Vec<Work>> = Mutex::new(Vec::new());

/// Handles this thread's native events until `timeout` passes (never, if `None`) or
/// [`wake`] is called, whichever is first; then runs the work posted for this thread.
/// Call it from the main thread.
pub fn wait(timeout: Option<Duration>) {
    imp::wait(timeout);
    run_posted();
}

/// Ends the current or the next [`wait`] early. Callable from any thread.
pub fn wake() {
    imp::wake();
}

/// Runs `work` on the thread that calls [`wait`], during its next wait.
#[cfg_attr(
    target_os = "linux",
    allow(dead_code, reason = "the Linux tray needs no thread")
)]
pub(crate) fn post(work: Work) {
    POSTED.lock().expect("poisoned").push(work);
    wake();
}

fn run_posted() {
    let work = std::mem::take(&mut *POSTED.lock().expect("poisoned"));
    for work in work {
        work();
    }
}

#[cfg_attr(target_os = "macos", path = "macos/main_thread.rs")]
#[cfg_attr(windows, path = "windows/main_thread.rs")]
#[cfg_attr(not(any(target_os = "macos", windows)), path = "other/main_thread.rs")]
mod imp;

// macOS: test threads are not the main thread, which AppKit insists on.
#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn wake_ends_a_wait_and_runs_posted_work() {
        let (tx, rx) = std::sync::mpsc::channel();
        post(Box::new(move || tx.send(()).unwrap()));
        let started = Instant::now();
        wait(Some(Duration::from_secs(30)));
        assert!(started.elapsed() < Duration::from_secs(10));
        rx.try_recv().unwrap();
        // A timeout alone also ends it.
        wait(Some(Duration::from_millis(10)));
    }
}
