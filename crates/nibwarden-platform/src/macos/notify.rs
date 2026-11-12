use std::process::{Command, Stdio};

/// `osascript` shows the notification (attributed to Script Editor, and silenced if the
/// user turned its notifications off). The text goes in as arguments, not into the
/// script, so quotes or backslashes in an item's name cannot become AppleScript. (The
/// summary is nibwarden's own text; options are only parsed before the first plain
/// argument, so the body, which may carry vault data, cannot be taken for one.)
pub fn notify(summary: &str, body: &str) {
    let spawned = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            summary,
            body,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        // Reap it in the background; the result does not matter.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => tracing::debug!(error = %e, "osascript unavailable"),
    }
}
