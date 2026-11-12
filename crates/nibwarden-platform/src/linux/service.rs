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
