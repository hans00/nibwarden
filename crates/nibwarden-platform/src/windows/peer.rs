//! Identifying the process at the other end of a named pipe.
//!
//! Compared with Linux: the process is opened by the pid the pipe reports and the handle is
//! kept, which pins it (a pid can be reused, a handle to the process cannot); but unlike
//! `SO_PEERPIDFD` the pipe cannot vouch that the pid still belongs to the client when we
//! open it. A client that connects and exits within microseconds could in theory have its
//! pid taken by another process of the same user before we look; the user check below
//! still holds then, and the liveness check at the end catches the common case of the
//! client having gone. There is no controlling terminal on Windows, so [`Peer::terminal`]
//! is `None` (approvals "per terminal" fall back to per application), and the peer's
//! environment is not read, so [`Peer::ssh_connection`] is `None`.

use std::{
    collections::HashMap,
    io,
    os::windows::{
        ffi::OsStringExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    },
    path::PathBuf,
};

use windows::{
    Win32::{
        Foundation::{FILETIME, HANDLE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Pipes::{
                GetNamedPipeClientProcessId, GetNamedPipeClientSessionId,
                GetNamedPipeServerProcessId, GetNamedPipeServerSessionId,
            },
            RemoteDesktop::{
                WTS_CURRENT_SERVER_HANDLE, WTSClientProtocolType, WTSFreeMemory,
                WTSQuerySessionInformationW,
            },
            Threading::{
                GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
    },
    core::PWSTR,
};

use super::{App, Peer};
use crate::{
    transport::imp::Stream,
    winutil::{IoResult as _, Sid, current_user, process_user},
};

/// A handle to the process: it cannot be confused with a later one that reuses its pid.
pub type ProcessHandle = OwnedHandle;

/// Programs that start the user's programs but are not one the user would know as an
/// application: the walk up from a client stops below them. `sshd` is also what marks a
/// login over SSH.
const BOUNDARIES: [&str; 17] = [
    "explorer.exe",
    "services.exe",
    "svchost.exe",
    "sshd.exe",
    "sshd-session.exe",
    "wininit.exe",
    "winlogon.exe",
    "userinit.exe",
    "csrss.exe",
    "smss.exe",
    "system",
    "lsass.exe",
    "sihost.exe",
    "taskhostw.exe",
    "runtimebroker.exe",
    "dllhost.exe",
    "ctfmon.exe",
];
const SSHD: [&str; 2] = ["sshd.exe", "sshd-session.exe"];

/// An opened process.
pub(super) struct Process {
    pid: u32,
    handle: OwnedHandle,
}

impl Process {
    pub(super) fn open(pid: u32) -> io::Result<Self> {
        // SAFETY: the new handle is owned from here on.
        let handle = unsafe {
            OwnedHandle::from_raw_handle(
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                    .io()?
                    .0,
            )
        };
        Ok(Self { pid, handle })
    }

    fn raw(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle())
    }

    fn user(&self) -> io::Result<Sid> {
        process_user(self.raw())
    }

    /// Creation time (100 ns since 1601), which tells this process from a later one with
    /// the same pid, and a parent from an unrelated process that took its pid.
    fn started(&self) -> io::Result<u64> {
        let mut times = [FILETIME::default(); 4];
        // SAFETY: four distinct FILETIMEs to write.
        unsafe {
            let [creation, exit, kernel, user] = &mut times;
            GetProcessTimes(self.raw(), creation, exit, kernel, user).io()?;
        }
        Ok((u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
    }

    fn exe(&self) -> Option<PathBuf> {
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` has `len` units.
        unsafe {
            QueryFullProcessImageNameW(
                self.raw(),
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .ok()?;
        }
        Some(PathBuf::from(std::ffi::OsString::from_wide(
            &buf[..len as usize],
        )))
    }

    fn alive(&self) -> bool {
        let mut code = 0u32;
        // SAFETY: `code` is written.
        let ok = unsafe { GetExitCodeProcess(self.raw(), &mut code) }.is_ok();
        // STILL_ACTIVE
        ok && code == 259
    }
}

pub fn identify_owner(stream: &Stream) -> io::Result<Option<Peer>> {
    let pipe = HANDLE(stream.raw_handle());
    let (mut pid, mut session) = (0u32, 0u32);
    // SAFETY: `pipe` is the open pipe handle of the stream; the outputs are written.
    unsafe {
        match stream {
            Stream::Server(_) => {
                GetNamedPipeClientProcessId(pipe, &mut pid).io()?;
                GetNamedPipeClientSessionId(pipe, &mut session).io()?;
            }
            Stream::Client(_) => {
                GetNamedPipeServerProcessId(pipe, &mut pid).io()?;
                GetNamedPipeServerSessionId(pipe, &mut session).io()?;
            }
        }
    }
    let process = Process::open(pid)?;
    let exe = process.exe();
    if process.user()? != current_user()? {
        tracing::warn!(
            pid,
            peer = exe
                .as_ref()
                .map_or_else(|| "?".into(), |p| p.display().to_string()),
            "rejected peer of another user"
        );
        return Ok(None);
    }

    let (app, via_sshd) = application(&process);
    let mut peer = Peer {
        pid: i32::try_from(pid).ok(),
        exe,
        ssh_connection: None,
        remote_session: via_sshd || session_is_remote(session),
        terminal: None,
        app,
        _pin: None,
    };
    // If the process is gone, what was read may be about another process.
    if !process.alive() {
        peer.exe = None;
        peer.remote_session = false;
        peer.app = None;
    }
    peer._pin = Some(process.handle);
    Ok(Some(peer))
}

/// Walks up the parents while they are the user's own ordinary programs. A parent is only
/// a parent if it started before its child: parent pids outlive the parent, and are
/// reused. Returns the application, and whether an `sshd` was met on the way.
pub(super) fn application(start: &Process) -> (Option<App>, bool) {
    let Some(parents) = parents() else {
        return (None, false);
    };
    let Ok(user) = current_user() else {
        return (None, false);
    };
    let mut current = Process {
        pid: start.pid,
        // A second handle to the same process.
        handle: match start.handle.try_clone() {
            Ok(handle) => handle,
            Err(_) => return (None, false),
        },
    };
    let mut via_sshd = false;
    for _ in 0..64 {
        let Some(&(ppid, _)) = parents.get(&current.pid) else {
            break;
        };
        let Ok(parent) = Process::open(ppid) else {
            break;
        };
        let (Ok(child_start), Ok(parent_start)) = (current.started(), parent.started()) else {
            break;
        };
        if parent_start > child_start || !parent.alive() {
            break;
        }
        let name = parents
            .get(&parent.pid)
            .map(|(_, name)| name.clone())
            .unwrap_or_default();
        if SSHD.contains(&name.as_str()) {
            via_sshd = true;
        }
        if BOUNDARIES.contains(&name.as_str()) || parent.user().ok().as_ref() != Some(&user) {
            break;
        }
        current = parent;
    }
    let app = current.started().ok().map(|started| App {
        pid: i32::try_from(current.pid).unwrap_or(i32::MAX),
        started,
        exe: current.exe(),
    });
    (app, via_sshd)
}

/// Every process's parent pid and lower-case executable name, from a snapshot.
fn parents() -> Option<HashMap<u32, (u32, String)>> {
    // SAFETY: the snapshot handle is owned from here on; the entry is initialised with its
    // size as the API demands.
    unsafe {
        let snapshot =
            OwnedHandle::from_raw_handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?.0);
        let handle = HANDLE(snapshot.as_raw_handle());
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut map = HashMap::new();
        let mut more = Process32FirstW(handle, &mut entry).is_ok();
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
            map.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
            more = Process32NextW(handle, &mut entry).is_ok();
        }
        Some(map)
    }
}

/// Whether the session the client runs in is not the user sitting at the machine: a
/// Remote Desktop (or other remote) session, or session 0, where services run and no
/// window can be seen. A login over SSH is recognised by its `sshd` ancestor instead.
fn session_is_remote(session: u32) -> bool {
    if session == 0 {
        return true;
    }
    let mut buffer = PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: on success the buffer holds a USHORT (`WTSClientProtocolType`), and is freed.
    unsafe {
        if WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            session,
            WTSClientProtocolType,
            &mut buffer,
            &mut bytes,
        )
        .is_err()
        {
            return false;
        }
        // 0: the console; 1: ICA; 2: RDP.
        let remote = bytes as usize >= size_of::<u16>() && *buffer.0.cast::<u16>() != 0;
        WTSFreeMemory(buffer.0.cast());
        remote
    }
}
