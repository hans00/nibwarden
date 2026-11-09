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

#[cfg(target_os = "linux")]
mod imp {
    use std::process::Command;

    /// `systemctl --user enable` (the unit is wanted by default.target).
    pub fn set_start_at_login(on: bool) -> std::io::Result<()> {
        let verb = if on { "enable" } else { "disable" };
        let out = Command::new("systemctl")
            .args(["--user", verb, "nibwardend.service"])
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "systemctl --user {verb}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    pub fn starts_at_login() -> bool {
        Command::new("systemctl")
            .args(["--user", "is-enabled", "--quiet", "nibwardend.service"])
            .status()
            .is_ok_and(|s| s.success())
    }

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

    pub fn set_start_at_login(_: bool) -> std::io::Result<()> {
        Err(crate::unsupported())
    }

    pub fn starts_at_login() -> bool {
        false
    }
}
