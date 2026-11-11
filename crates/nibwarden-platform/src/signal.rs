//! Requests to shut down.

/// Resolves when the process is asked to stop (Linux: SIGTERM or SIGINT).
pub async fn shutdown_requested() -> std::io::Result<()> {
    imp::shutdown_requested().await
}

#[cfg_attr(target_os = "linux", path = "linux/signal.rs")]
#[cfg_attr(target_os = "macos", path = "macos/signal.rs")]
#[cfg_attr(windows, path = "windows/signal.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/signal.rs"
)]
mod imp;
