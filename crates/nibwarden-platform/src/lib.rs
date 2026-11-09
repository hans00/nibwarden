//! Platform layer (phase2.md §8). Everything that differs between operating systems lives
//! here; the other crates use only these interfaces and contain no `cfg(target_os)`.
//!
//! Linux is implemented. Other targets compile, so interfaces cannot quietly grow
//! Linux-only types, but their functions report [`std::io::ErrorKind::Unsupported`] until
//! they are ported.

pub mod autostart;
pub mod browser;
#[cfg(feature = "clipboard")]
pub mod clipboard;
pub mod clock;
pub mod display;
pub mod fs;
pub mod hardening;
pub mod locale;
pub mod native_messaging;
pub mod notify;
pub mod paths;
pub mod peer;
pub mod polkit;
pub mod process;
pub mod service;
pub mod session_events;
pub mod shortcut;
pub mod signal;
pub mod terminal;
pub mod transport;
#[cfg(feature = "tray")]
pub mod tray;

#[cfg(not(target_os = "linux"))]
fn unsupported() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "nibwarden does not support this platform yet",
    )
}
