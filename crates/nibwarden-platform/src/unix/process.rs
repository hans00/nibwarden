use std::{io, os::unix::process::CommandExt as _, process::Command};

pub fn exec(command: &mut Command) -> io::Error {
    command.exec()
}
