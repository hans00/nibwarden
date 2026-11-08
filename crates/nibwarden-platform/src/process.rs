//! Handing over to another program (`nibwarden run`, `nibwarden-ssh-sign`).

use std::{io, process::Command};

/// Runs `command` in place of this process, so its exit status becomes ours. Returns only
/// if that failed.
pub fn exec(command: &mut Command) -> io::Error {
    imp::exec(command)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{io, os::unix::process::CommandExt as _, process::Command};

    pub fn exec(command: &mut Command) -> io::Error {
        command.exec()
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::{io, process::Command};

    /// Windows has no exec; the port will spawn, wait and exit with the child's code.
    pub fn exec(_: &mut Command) -> io::Error {
        crate::unsupported()
    }
}
