//! LaunchAgents in `~/Library/LaunchAgents`: the property list files and `launchctl`.

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::macos_logic::launch_agent_plist;

/// Reverse-DNS prefix of every label nibwarden gives launchd.
pub const LABEL_PREFIX: &str = "me.hans00.nibwarden";

const LAUNCHCTL: &str = "/bin/launchctl";

/// `~/Library/LaunchAgents`, where launchd looks for the user's agents at login.
fn agents_dir() -> io::Result<PathBuf> {
    Ok(crate::paths::imp::home()?.join("Library/LaunchAgents"))
}

pub fn plist_path(label: &str) -> io::Result<PathBuf> {
    Ok(agents_dir()?.join(format!("{label}.plist")))
}

/// `~/Library/Logs/nibwarden/<name>.log`, where the agent's output goes (there is no
/// journal); the directory is created.
pub fn log_path(name: &str) -> Option<PathBuf> {
    let dir = crate::paths::imp::home()
        .ok()?
        .join("Library/Logs/nibwarden");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{name}.log")))
}

/// Writes the agent's property list if it differs from what is there; whether it changed.
pub fn write_agent(
    label: &str,
    arguments: &[String],
    run_at_load: bool,
    log: Option<&Path>,
) -> io::Result<bool> {
    let path = plist_path(label)?;
    let text = launch_agent_plist(label, arguments, run_at_load, log);
    if std::fs::read_to_string(&path).is_ok_and(|current| current == text) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Replaced in one step: launchd may read the file at any moment.
    let partial = path.with_extension("plist.tmp");
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, &path)?;
    Ok(true)
}

/// The user's GUI domain, `gui/<uid>`: where agents of the login session live.
pub fn domain() -> String {
    // SAFETY: getuid has no failure mode.
    format!("gui/{}", unsafe { libc::getuid() })
}

/// `launchctl <args>`, with its exit status; output is discarded.
pub async fn launchctl(args: &[&str]) -> bool {
    tokio::process::Command::new(LAUNCHCTL)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}
