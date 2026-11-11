// Not ported yet: reports Unsupported like other platforms.
pub fn notify(summary: &str, _: &str) {
    tracing::debug!(summary, "notifications are not supported on this platform");
}
