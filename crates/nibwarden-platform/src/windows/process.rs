// Not ported yet: reports Unsupported like other platforms.
use std::{io, process::Command};

/// Windows has no exec; the port will spawn, wait and exit with the child's code.
pub fn exec(_: &mut Command) -> io::Error {
    crate::unsupported()
}
