//! Handing over to another program (`nibwarden run`, `nibwarden-ssh-sign`).

use std::{io, process::Command};

/// Runs `command` in place of this process, so its exit status becomes ours. Returns only
/// if that failed.
pub fn exec(command: &mut Command) -> io::Error {
    imp::exec(command)
}

#[cfg_attr(target_os = "linux", path = "linux/process.rs")]
#[cfg_attr(target_os = "macos", path = "macos/process.rs")]
#[cfg_attr(windows, path = "windows/process.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/process.rs"
)]
mod imp;
