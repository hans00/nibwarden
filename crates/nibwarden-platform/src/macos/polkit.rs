//! System authentication through LocalAuthentication: `LAPolicyDeviceOwnerAuthentication`
//! asks for Touch ID, an Apple Watch or the login password, in the system's own dialog.

use std::io;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAPolicy};

use crate::macos_logic::authentication_outcome;

/// The text the system dialog shows (it names the program asking).
const REASON: &str = "unlock your nibwarden vault";

/// `canEvaluatePolicy:error:`: a way to authenticate the user is set up (every Mac with a
/// login password has one).
pub async fn available() -> bool {
    run(|| {
        // SAFETY: LAContext is created, used and dropped on this thread; the error out
        // parameter is handled by the generated binding.
        let context = unsafe { LAContext::new() };
        Ok(unsafe { context.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication) }.is_ok())
    })
    .await
    .unwrap_or(false)
}

/// `evaluatePolicy:localizedReason:reply:`. The process is not needed: the dialog appears
/// in the user's own login session.
pub async fn authenticate(_pid: u32) -> io::Result<bool> {
    run(|| {
        // SAFETY: as above; the reply block is copied by the framework and only sends
        // on a channel.
        let context = unsafe { LAContext::new() };
        let (tx, rx) = std::sync::mpsc::channel();
        let reply = RcBlock::new(move |success: Bool, error: *mut NSError| {
            // SAFETY: when not null, `error` is a valid NSError for this call.
            let code = unsafe { error.as_ref() }.map(NSError::code);
            let _ = tx.send(authentication_outcome(success.as_bool(), code));
        });
        let reason = NSString::from_str(REASON);
        unsafe {
            context.evaluatePolicy_localizedReason_reply(
                LAPolicy::DeviceOwnerAuthentication,
                &reason,
                &reply,
            );
        }
        // The context lives until the reply arrived (it would cancel the evaluation when
        // released).
        let outcome = rx.recv();
        drop(context);
        outcome.unwrap_or_else(|_| Err(io::Error::other("authentication was abandoned")))
    })
    .await
}

/// Runs `work` on a thread of its own (the Objective-C objects stay on it) and waits for
/// the result without blocking the async runtime.
async fn run<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("system-authentication".into())
        .spawn(move || {
            let _ = tx.send(work());
        })?;
    rx.await
        .unwrap_or_else(|_| Err(io::Error::other("authentication thread ended")))
}
