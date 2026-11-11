//! Events that should lock the vault (phase2.md §4.3): the screen locking and the machine
//! going to sleep.

/// Something happened to the user's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// One of the user's sessions locked its screen.
    ScreenLocked,
    /// The machine is about to suspend or hibernate.
    Sleeping,
}

/// Sends events until the event source goes away. Fails if it cannot be reached at all
/// (e.g. no system bus in a container); the caller then simply goes without.
pub async fn watch(
    events: tokio::sync::mpsc::UnboundedSender<SessionEvent>,
) -> std::io::Result<()> {
    imp::watch(events).await
}

#[cfg_attr(target_os = "linux", path = "linux/session_events.rs")]
#[cfg_attr(target_os = "macos", path = "macos/session_events.rs")]
#[cfg_attr(windows, path = "windows/session_events.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/session_events.rs"
)]
mod imp;
