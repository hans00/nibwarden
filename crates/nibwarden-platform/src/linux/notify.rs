use std::process::Stdio;

/// Through `notify-send` for now; phase2.md §8 moves this to D-Bus with actions.
pub fn notify(summary: &str, body: &str) {
    let spawned = tokio::process::Command::new("notify-send")
        .args(["--app-name=nibwarden", "--icon=dialog-password"])
        .arg(summary)
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        // Reap it in the background; the result does not matter.
        Ok(mut child) => {
            tokio::spawn(async move { child.wait().await });
        }
        Err(e) => tracing::debug!(error = %e, "notify-send unavailable"),
    }
}
