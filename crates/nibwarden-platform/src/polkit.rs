//! Confirming that the user is at the computer with the system's own authentication (the
//! login password, or a fingerprint where the system is set up for it), for unlocking
//! without the master password (phase2.md §12.19). Linux: polkit, with the action
//! nibwarden's packages install; the desktop's polkit agent shows the dialog.

use std::io;

/// The polkit action: `packaging/polkit/me.hans00.nibwarden.policy`.
pub const ACTION: &str = "me.hans00.nibwarden.unlock";

/// Whether the system can confirm the user for nibwarden (the action is installed).
/// Asked often (every status request); once found, the action is taken to stay.
pub async fn available() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FOUND: AtomicBool = AtomicBool::new(false);
    if FOUND.load(Ordering::Relaxed) {
        return true;
    }
    let found = imp::available().await;
    FOUND.store(found, Ordering::Relaxed);
    found
}

/// Asks the system to confirm the user, for the process `pid` (whose session's agent
/// shows the dialog); `true` if confirmed, `false` if cancelled or refused.
pub async fn authenticate(pid: u32) -> io::Result<bool> {
    imp::authenticate(pid).await
}

#[cfg_attr(target_os = "linux", path = "linux/polkit.rs")]
#[cfg_attr(target_os = "macos", path = "macos/polkit.rs")]
#[cfg_attr(windows, path = "windows/polkit.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/polkit.rs"
)]
mod imp;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::collections::HashMap;

    /// What polkitd answers after the user confirmed, and when they did not.
    #[test]
    fn reads_the_authorization_result() {
        let call = zbus::Message::method_call(
            "/org/freedesktop/PolicyKit1/Authority",
            "CheckAuthorization",
        )
        .unwrap()
        .build(&())
        .unwrap();
        for (authorized, details) in [
            (true, HashMap::new()),
            (false, HashMap::from([("polkit.dismissed", "true")])),
        ] {
            let reply = zbus::Message::method_return(&call.header())
                .unwrap()
                .build(&((authorized, !authorized, details),))
                .unwrap();
            assert_eq!(super::imp::authorized(&reply).unwrap(), authorized);
        }
    }
}
