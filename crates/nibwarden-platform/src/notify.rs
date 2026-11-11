//! Best-effort desktop notifications.

/// Shows a notification if the session has a notification service; failures are only
/// logged (a headless SSH login has none).
pub fn notify(summary: &str, body: &str) {
    imp::notify(summary, body);
}

#[cfg_attr(target_os = "linux", path = "linux/notify.rs")]
#[cfg_attr(target_os = "macos", path = "macos/notify.rs")]
#[cfg_attr(windows, path = "windows/notify.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/notify.rs"
)]
mod imp;
