//! Platform layer (phase2.md §8). Everything that differs between operating systems lives
//! here; the other crates use only these interfaces and contain no `cfg(target_os)`.
//!
//! Each module keeps its interface here and one implementation per platform in
//! `linux/`, `macos/`, `windows/` and `other/` (the last reports
//! [`std::io::ErrorKind::Unsupported`], so interfaces cannot quietly grow types only one
//! platform has).

pub mod autostart;
pub mod browser;
#[cfg(feature = "clipboard")]
pub mod clipboard;
pub mod clock;
pub mod display;
pub mod fs;
pub mod hardening;
pub mod locale;
#[cfg(feature = "tray")]
pub mod main_thread;
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

// The macOS port's parsing and formatting; compiled everywhere so its tests run everywhere.
#[allow(dead_code, reason = "used on macOS only")]
#[path = "macos/logic.rs"]
mod macos_logic;

#[allow(dead_code, reason = "used only where a platform is not ported")]
#[cfg(not(target_os = "linux"))]
fn unsupported() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "nibwarden does not support this platform yet",
    )
}
