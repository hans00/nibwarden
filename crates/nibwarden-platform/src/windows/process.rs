use std::{io, process::Command};

use windows::Win32::System::Console::SetConsoleCtrlHandler;

/// Windows has no `exec`: runs `command` with the standard streams inherited, waits for it,
/// and exits with its status (1 if it has none).
///
/// Ctrl-C reaches the child and us alike (same console); by default it would end us while
/// the child is still cleaning up, so we ignore it until the child is done.
pub fn exec(command: &mut Command) -> io::Error {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return e,
    };
    // SAFETY: no handler routine with `add` set makes this process ignore Ctrl-C.
    let _ = unsafe { SetConsoleCtrlHandler(None, true) };
    match child.wait() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("cannot wait for the program: {e}");
            std::process::exit(1)
        }
    }
}
