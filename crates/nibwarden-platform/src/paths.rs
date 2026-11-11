//! Where nibwarden keeps its files.

use std::{io, path::PathBuf};

/// Directory (private to the user) for the daemon's sockets; `None` if the platform's
/// per-session runtime directory is unknown.
pub fn runtime_dir() -> Option<PathBuf> {
    imp::runtime_dir()
}

/// Session, encrypted vault cache and other persistent state.
pub fn data_dir() -> io::Result<PathBuf> {
    imp::data_dir()
}

/// User settings (`config.toml`).
pub fn config_dir() -> io::Result<PathBuf> {
    imp::config_dir()
}

#[cfg_attr(target_os = "linux", path = "linux/paths.rs")]
#[cfg_attr(target_os = "macos", path = "macos/paths.rs")]
#[cfg_attr(windows, path = "windows/paths.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/paths.rs"
)]
pub(crate) mod imp;
