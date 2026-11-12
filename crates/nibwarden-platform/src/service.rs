//! Starting the daemon on demand, or at every login, through the platform's service
//! manager.

/// Asks the service manager to start `nibwardend`; `true` once it reports success. The
/// caller still has to wait for the socket to appear.
pub async fn start_daemon() -> bool {
    imp::start_daemon().await
}

/// Starts `nibwardend` at every login from now on (live sync, the SSH agent ready), or
/// only on demand again.
pub fn set_start_at_login(on: bool) -> std::io::Result<()> {
    imp::set_start_at_login(on)
}

/// Whether `nibwardend` starts at login.
pub fn starts_at_login() -> bool {
    imp::starts_at_login()
}

#[cfg_attr(target_os = "linux", path = "linux/service.rs")]
#[cfg_attr(target_os = "macos", path = "macos/service.rs")]
#[cfg_attr(windows, path = "windows/service.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/service.rs"
)]
mod imp;
