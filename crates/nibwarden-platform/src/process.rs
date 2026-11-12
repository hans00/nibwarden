//! Handing over to another program (`nibwarden run`, `nibwarden-ssh-sign`).

use std::{io, process::Command};

/// Runs `command` in place of this process, so its exit status becomes ours. Returns only
/// if that failed.
pub fn exec(command: &mut Command) -> io::Error {
    imp::exec(command)
}

#[cfg_attr(
    any(target_os = "linux", target_os = "macos"),
    path = "unix/process.rs"
)]
#[cfg_attr(windows, path = "windows/process.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/process.rs"
)]
mod imp;
