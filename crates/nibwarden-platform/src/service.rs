//! Starting the daemon on demand through the platform's service manager.

/// Asks the service manager to start `nibwardend`; `true` once it reports success. The
/// caller still has to wait for the socket to appear.
pub async fn start_daemon() -> bool {
    imp::start_daemon().await
}

#[cfg(target_os = "linux")]
mod imp {
    /// `systemctl --user start nibwardend.service` (packaging/systemd).
    pub async fn start_daemon() -> bool {
        tokio::process::Command::new("systemctl")
            .args(["--user", "start", "nibwardend.service"])
            .status()
            .await
            .is_ok_and(|s| s.success())
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub async fn start_daemon() -> bool {
        false
    }
}
