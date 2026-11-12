use std::{
    ffi::OsString,
    io,
    os::{fd::AsRawFd as _, unix::ffi::OsStringExt as _},
    path::{Path, PathBuf},
};

use tokio::net::UnixStream;

use super::{App, Peer};
use crate::macos_logic::{
    is_boundary_program, is_remote_login_program, procargs2_env, start_micros, terminal_id,
};

/// What pins the identity to one process: its pid and start time, read before and after
/// everything else about it, and the pid version of its audit token, which changes when
/// the kernel reuses a pid. macOS has no pidfd, so this is as close as it gets.
#[derive(Debug)]
#[allow(dead_code, reason = "kept for the connection's lifetime, as a record")]
pub struct ProcessHandle {
    pid: i32,
    started: u64,
    pid_version: Option<u32>,
}

/// What `proc_pidinfo(PROC_PIDTBSDINFO)` says about a process, reduced to what we use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Info {
    uid: u32,
    ppid: i32,
    tdev: u32,
    started: u64,
    comm: [u8; 16],
}

impl Info {
    fn comm(&self) -> String {
        let end = self.comm.iter().position(|&b| b == 0).unwrap_or(16);
        String::from_utf8_lossy(&self.comm[..end]).into_owned()
    }
}

fn info(pid: i32) -> Option<Info> {
    if pid <= 0 {
        return None;
    }
    // SAFETY: an all-zero proc_bsdinfo is a valid out-buffer.
    let mut raw: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `raw`.
    let written =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut raw).cast(), size) };
    // The call returns the size it filled; anything else (0: no such process or not
    // allowed to look) means there is no answer.
    if written != size || raw.pbi_pid != pid as u32 {
        return None;
    }
    let mut comm = [0u8; 16];
    for (to, from) in comm.iter_mut().zip(raw.pbi_comm) {
        *to = from as u8;
    }
    Some(Info {
        uid: raw.pbi_uid,
        ppid: raw.pbi_ppid as i32,
        tdev: raw.e_tdev,
        started: start_micros(raw.pbi_start_tvsec, raw.pbi_start_tvusec),
        comm,
    })
}

/// `proc_pidpath`: the executable of a process.
fn exe(pid: i32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: proc_pidpath writes at most `buffersize` bytes into `buf`.
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let len = usize::try_from(len).ok().filter(|&l| l > 0)?;
    buf.truncate(len);
    Some(PathBuf::from(OsString::from_vec(buf)))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// `LOCAL_PEERTOKEN`: the audit token of the process that connected, as eight words;
/// word 5 is its pid and word 7 its pid version.
fn peer_token(socket: libc::c_int) -> Option<[u32; 8]> {
    let mut token = [0u32; 8];
    let mut len = size_of::<[u32; 8]>() as libc::socklen_t;
    // SAFETY: getsockopt writes at most `len` bytes into `token`.
    let rc = unsafe {
        libc::getsockopt(
            socket,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            (&raw mut token).cast(),
            &mut len,
        )
    };
    (rc == 0 && len as usize == size_of::<[u32; 8]>()).then_some(token)
}

/// The environment variable `name` of a process (`KERN_PROCARGS2`).
fn environment_var(pid: i32, name: &str) -> Option<String> {
    let mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    for _ in 0..3 {
        let mut size = 0usize;
        // SAFETY: with a null buffer sysctl only reports the size needed.
        let rc = unsafe {
            libc::sysctl(
                mib.as_ptr().cast_mut(),
                3,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || size == 0 {
            return None;
        }
        // The environment may have grown since the size was read.
        let mut buf = vec![0u8; size + 4096];
        let mut len = buf.len();
        // SAFETY: sysctl writes at most `len` bytes into `buf` and updates `len`.
        let rc = unsafe {
            libc::sysctl(
                mib.as_ptr().cast_mut(),
                3,
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            buf.truncate(len);
            return procargs2_env(&buf, name);
        }
        if io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
            return None;
        }
    }
    None
}

pub fn identify_owner(stream: &UnixStream) -> io::Result<Option<Peer>> {
    let cred = stream.peer_cred()?;
    let pid = cred.pid();
    let mut peer = Peer {
        pid,
        exe: None,
        ssh_connection: None,
        remote_session: false,
        terminal: None,
        app: None,
        _pin: None,
    };
    // SAFETY: geteuid has no failure mode.
    let me = unsafe { libc::geteuid() };
    if cred.uid() != me {
        peer.exe = pid.and_then(exe);
        tracing::warn!(
            uid = cred.uid(),
            peer = peer.describe(),
            "rejected peer with foreign uid"
        );
        return Ok(None);
    }
    let Some(pid) = pid else {
        return Ok(Some(peer));
    };

    let token = peer_token(stream.as_raw_fd());
    if let Some(token) = token
        && token[5] != pid as u32
    {
        tracing::debug!(pid, token_pid = token[5], "peer pid and audit token differ");
    }

    // Everything below describes "the process with this pid"; it is only kept if the
    // process (told apart from a successor by its start time and owner) is the same
    // before and after.
    let Some(before) = info(pid).filter(|i| i.uid == me) else {
        return Ok(Some(peer));
    };
    peer.exe = exe(pid);
    peer.terminal = terminal_id(before.tdev);
    peer.app = application(pid, &before);
    peer.remote_session = remote_login_ancestor(&before);
    peer.ssh_connection = environment_var(pid, "SSH_CONNECTION");

    if info(pid) == Some(before) {
        peer._pin = Some(ProcessHandle {
            pid,
            started: before.started,
            pid_version: token.map(|t| t[7]),
        });
    } else {
        // The process is gone, and its pid may describe someone else already.
        peer.exe = None;
        peer.ssh_connection = None;
        peer.remote_session = false;
        peer.terminal = None;
        peer.app = None;
    }
    Ok(Some(peer))
}

/// The topmost ancestor (or the process itself) below the service manager (`launchd`,
/// pid 1), a login service (`sshd`, `sshd-session`) or a process whose executable cannot
/// be read: Terminal.app, iTerm2, an IDE, or a shell started from a login.
fn application(pid: i32, start: &Info) -> Option<App> {
    let boundary =
        |pid: i32| pid <= 1 || exe(pid).is_none_or(|exe| is_boundary_program(&file_name(&exe)));
    let (mut current, mut current_info) = (pid, *start);
    for _ in 0..64 {
        let parent = current_info.ppid;
        if boundary(parent) {
            break;
        }
        let Some(parent_info) = info(parent) else {
            break;
        };
        (current, current_info) = (parent, parent_info);
    }
    Some(App {
        pid: current,
        started: current_info.started,
        exe: exe(current),
    })
}

/// Whether a remote login service is among the ancestors: the session the peer runs in
/// was started by `sshd`. A process that detached from its session (reparented to
/// `launchd`) is not seen as remote.
fn remote_login_ancestor(start: &Info) -> bool {
    let mut current = *start;
    for _ in 0..64 {
        if current.ppid <= 1 {
            return false;
        }
        let Some(parent) = info(current.ppid) else {
            return false;
        };
        let name = exe(current.ppid).map_or_else(|| parent.comm(), |e| file_name(&e));
        if is_remote_login_program(&name) || is_remote_login_program(&parent.comm()) {
            return true;
        }
        current = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_own_process_info() {
        let pid = std::process::id() as i32;
        let me = info(pid).unwrap();
        // SAFETY: getppid/geteuid have no failure mode.
        assert_eq!(me.ppid, unsafe { libc::getppid() });
        assert_eq!(me.uid, unsafe { libc::geteuid() });
        assert!(me.started > 1_600_000_000_000_000);
        assert_eq!(
            exe(pid).map(|p| p.canonicalize().unwrap()),
            Some(std::env::current_exe().unwrap().canonicalize().unwrap())
        );
        assert_eq!(info(0), None);
        assert_eq!(info(-5), None);
    }

    #[test]
    fn reads_own_environment() {
        // The environment is captured at exec, so only variables that were there then
        // are visible; PATH, which nothing here changes, is one.
        if let Ok(path) = std::env::var("PATH") {
            assert_eq!(
                environment_var(std::process::id() as i32, "PATH"),
                Some(path)
            );
        }
    }
}
