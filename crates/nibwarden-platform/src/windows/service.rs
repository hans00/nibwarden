//! No service manager: the daemon is an ordinary program of the user, started detached on
//! demand and, if wanted, from the `Run` key at login.

use std::{
    io,
    os::windows::process::CommandExt as _,
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::winutil::run_key;

/// `DETACHED_PROCESS`: no console, so closing the terminal that started it does not end it.
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP`: Ctrl-C in the starting terminal does not reach it.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// `CREATE_NO_WINDOW` (ignored together with `DETACHED_PROCESS`, but harmless).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `CREATE_BREAKAWAY_FROM_JOB`: a terminal or an IDE may kill the jobs it started; the
/// daemon should outlive them. Refused (access denied) if the job does not allow it.
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

const RUN_NAME: &str = "nibwardend";

/// `nibwardend.exe` next to this program, else the one on `PATH`.
fn daemon_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name("nibwardend.exe"))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("nibwardend.exe"))
}

fn spawn(exe: &PathBuf, breakaway: bool) -> io::Result<()> {
    let mut flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    if breakaway {
        flags |= CREATE_BREAKAWAY_FROM_JOB;
    }
    // Not waited for and not killed on drop: the handle is just closed.
    Command::new(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(flags)
        .spawn()
        .map(drop)
}

/// Starts `nibwardend.exe` detached. `true` once the process exists; whether it listens
/// yet is for the caller to find out. A daemon that runs already makes the new one exit
/// on finding its pipe taken.
pub async fn start_daemon() -> bool {
    tokio::task::spawn_blocking(|| {
        let exe = daemon_exe();
        match spawn(&exe, true) {
            Ok(()) => true,
            // Not allowed to break away from the job we are in.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => spawn(&exe, false).is_ok(),
            Err(e) => {
                tracing::debug!(error = %e, exe = %exe.display(), "cannot start nibwardend");
                false
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// A `Run` value for `nibwardend.exe`: the daemon starts when the user logs in.
pub fn set_start_at_login(on: bool) -> io::Result<()> {
    let exe = daemon_exe();
    run_key::set(RUN_NAME, &[exe.display().to_string()], on)
}

pub fn starts_at_login() -> bool {
    run_key::is_set(RUN_NAME)
}
