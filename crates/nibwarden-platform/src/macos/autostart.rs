use std::io;

use crate::launchd::{self, LABEL_PREFIX};

/// `me.hans00.nibwarden.<name>`: an agent of its own, apart from the daemon's.
fn label(name: &str) -> String {
    format!("{LABEL_PREFIX}.{name}")
}

/// A LaunchAgent with `RunAtLoad` (`~/Library/LaunchAgents/<label>.plist`), which launchd
/// loads and starts at the next login; removed to turn it off. The agent running now is
/// left alone either way. `title` is not used: macOS names a login item after its
/// program.
pub fn set(name: &str, _title: &str, command: &[String], on: bool) -> io::Result<()> {
    let label = label(name);
    if !on {
        return match std::fs::remove_file(launchd::plist_path(&label)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    launchd::write_agent(&label, command, true, launchd::log_path(name).as_deref()).map(drop)
}

pub fn is_set(name: &str) -> bool {
    launchd::plist_path(&label(name)).is_ok_and(|path| path.exists())
}
