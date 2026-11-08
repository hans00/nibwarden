//! Who is on the other end of a connection (phase1.md §6.4).

use std::path::PathBuf;

use crate::transport::Stream;

#[derive(Debug)]
pub struct Peer {
    pub pid: Option<i32>,
    /// Executable of the peer, for display and policy. `None` if it could not be read, or
    /// if the process exited while we were looking (its pid may be reused).
    pub exe: Option<PathBuf>,
    /// `SSH_CONNECTION` from the peer's environment. The peer controls its environment,
    /// so this is a display hint only, never a security decision.
    pub ssh_connection: Option<String>,
    /// The peer belongs to a remote login (e.g. over SSH), as the session manager records
    /// it. Windows for such a peer must not appear on the local desktop (phase2.md §6).
    /// Unlike `ssh_connection` the peer cannot change this.
    pub remote_session: bool,
    /// The peer's controlling terminal, as an opaque id: commands typed in one terminal
    /// share it. `None` without a terminal (e.g. started from a desktop launcher).
    pub terminal: Option<u64>,
    /// The application the request comes from: the topmost of the peer's ancestors that
    /// still belongs to the user's own programs (below the service manager), e.g. the
    /// terminal emulator a command was typed in, or the IDE. Approvals are remembered per
    /// application, like 1Password's SSH agent does (phase2.md §4.2).
    pub app: Option<App>,
    /// Keeps the identity above pinned to one process for the connection's lifetime.
    _pin: Option<imp::ProcessHandle>,
}

impl Peer {
    /// Identifies the peer and keeps it only if it runs as the current user; anything
    /// else is logged and refused (phase1.md §6.4 step 1).
    pub fn identify_owner(stream: &Stream) -> Option<Self> {
        match imp::identify_owner(&stream.0) {
            Ok(Some(peer)) => Some(peer),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "cannot identify peer; dropping connection");
                None
            }
        }
    }

    /// A peer that was never connected, for tests of code that takes a `Peer`.
    pub fn synthetic(pid: i32, exe: Option<PathBuf>) -> Self {
        Self {
            pid: Some(pid),
            exe,
            ssh_connection: None,
            remote_session: false,
            terminal: None,
            app: None,
            _pin: None,
        }
    }

    /// Short name for display: the executable's file name, or `?`.
    pub fn name(&self) -> String {
        self.exe
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| "?".to_owned(), |n| n.to_string_lossy().into_owned())
    }

    /// Short description for logs and approval prompts.
    pub fn describe(&self) -> String {
        let exe = self
            .exe
            .as_ref()
            .map_or_else(|| "?".to_owned(), |p| p.display().to_string());
        match self.pid {
            Some(pid) => format!("{exe} (pid {pid})"),
            None => exe,
        }
    }
}

/// A running application, told apart from a later process with the same pid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct App {
    pub pid: i32,
    /// Start time of the process (clock ticks since boot), so a new process that reuses
    /// the pid is a different application.
    pub started: u64,
    pub exe: Option<PathBuf>,
}

impl App {
    pub fn name(&self) -> String {
        self.exe
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| "?".to_owned(), |n| n.to_string_lossy().into_owned())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{
        fs, io,
        os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
        path::PathBuf,
    };

    use tokio::net::UnixStream;

    use super::Peer;

    /// A pidfd: the process cannot be confused with a later one that reuses its pid.
    pub type ProcessHandle = OwnedFd;

    /// `SO_PEERPIDFD` (Linux 6.5+): a pidfd for the process that called `connect()`.
    const SO_PEERPIDFD: libc::c_int = 77;

    pub fn identify_owner(stream: &UnixStream) -> io::Result<Option<Peer>> {
        let cred = stream.peer_cred()?;
        let pid = cred.pid();
        let describe = |exe: &Option<PathBuf>| {
            Peer {
                pid,
                exe: exe.clone(),
                ssh_connection: None,
                remote_session: false,
                terminal: None,
                app: None,
                _pin: None,
            }
            .describe()
        };
        // SAFETY: geteuid has no failure mode.
        if cred.uid() != unsafe { libc::geteuid() } {
            tracing::warn!(
                uid = cred.uid(),
                peer = describe(&pid.and_then(|p| fs::read_link(format!("/proc/{p}/exe")).ok())),
                "rejected peer with foreign uid"
            );
            return Ok(None);
        }
        let mut peer = Peer {
            pid,
            exe: None,
            ssh_connection: None,
            remote_session: false,
            terminal: None,
            app: None,
            _pin: None,
        };
        let Some(pid) = pid else {
            return Ok(Some(peer));
        };

        // Prefer the pidfd the kernel took at connect time; pidfd_open(pid) is racy if the
        // client already exited and its pid was recycled.
        let pidfd = peer_pidfd(stream.as_raw_fd()).or_else(|| pidfd_open(pid));
        let proc = PathBuf::from(format!("/proc/{pid}"));
        peer.exe = fs::read_link(proc.join("exe")).ok();
        peer.app = application(pid);
        peer.terminal = fs::read_to_string(proc.join("stat"))
            .ok()
            .and_then(|stat| controlling_tty(&stat));
        peer.remote_session = fs::read_to_string(proc.join("sessionid"))
            .ok()
            .is_some_and(|id| session_is_remote(id.trim()));
        peer.ssh_connection = fs::read(proc.join("environ")).ok().and_then(|env| {
            env.split(|&b| b == 0)
                .find_map(|var| var.strip_prefix(b"SSH_CONNECTION="))
                .map(|v| String::from_utf8_lossy(v).into_owned())
        });

        // If the process is gone, /proc/<pid> may already describe someone else.
        if let Some(fd) = &pidfd
            && !pidfd_alive(fd.as_raw_fd())
        {
            peer.exe = None;
            peer.ssh_connection = None;
            peer.remote_session = false;
            peer.terminal = None;
            peer.app = None;
        }
        peer._pin = pidfd;
        Ok(Some(peer))
    }

    /// `tty_nr` from `/proc/<pid>/stat`; 0 means none. The command name in parentheses may
    /// itself contain spaces and parentheses, so fields are counted from the last `)`.
    pub(super) fn controlling_tty(stat: &str) -> Option<u64> {
        let tty: u64 = stat_field(stat, 4)?.parse().ok()?;
        (tty != 0).then_some(tty)
    }

    /// Field `index` after the command name: 0 = state, 1 = ppid, 4 = tty_nr,
    /// 19 = starttime.
    fn stat_field(stat: &str, index: usize) -> Option<&str> {
        stat[stat.rfind(')')? + 1..].split_whitespace().nth(index)
    }

    /// Walks up the parents while they are the user's own ordinary programs: stops below
    /// the user's service manager (`systemd --user`), a login service (`sshd`), init, or a
    /// process whose executable cannot be read (another user's).
    fn application(pid: i32) -> Option<super::App> {
        let stat = |pid: i32| fs::read_to_string(format!("/proc/{pid}/stat")).ok();
        let exe = |pid: i32| fs::read_link(format!("/proc/{pid}/exe")).ok();
        let boundary = |pid: i32| {
            pid <= 1
                || exe(pid).is_none_or(|exe| {
                    let name = exe
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    matches!(name.as_str(), "systemd" | "sshd" | "sshd-session" | "init")
                })
        };
        let mut current = pid;
        for _ in 0..64 {
            let parent: i32 = stat_field(&stat(current)?, 1)?.parse().ok()?;
            if boundary(parent) {
                break;
            }
            current = parent;
        }
        Some(super::App {
            pid: current,
            started: stat_field(&stat(current)?, 19)?.parse().ok()?,
            exe: exe(current),
        })
    }

    /// systemd-logind's record of an audit session (`/proc/<pid>/sessionid`, which only a
    /// privileged process can change): `REMOTE=1` for logins from another host.
    fn session_is_remote(id: &str) -> bool {
        // 4294967295 (unset): not part of any login session.
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || id == "4294967295" {
            return false;
        }
        fs::read_to_string(format!("/run/systemd/sessions/{id}"))
            .is_ok_and(|record| record.lines().any(|l| l == "REMOTE=1"))
    }

    fn peer_pidfd(socket: RawFd) -> Option<OwnedFd> {
        let mut fd: libc::c_int = -1;
        let mut len = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: getsockopt writes at most `len` bytes into `fd`.
        let rc = unsafe {
            libc::getsockopt(
                socket,
                libc::SOL_SOCKET,
                SO_PEERPIDFD,
                (&raw mut fd).cast(),
                &mut len,
            )
        };
        // SAFETY: on success the kernel handed us a new fd that nobody else owns.
        (rc == 0 && fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn pidfd_open(pid: i32) -> Option<OwnedFd> {
        // SAFETY: plain syscall; returns a new fd or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        // SAFETY: as above, a fresh fd owned by us.
        (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
    }

    fn pidfd_alive(pidfd: RawFd) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd,
                0,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        rc == 0
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::io;

    use super::Peer;

    pub enum ProcessHandle {}

    impl std::fmt::Debug for ProcessHandle {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match *self {}
        }
    }

    pub fn identify_owner(stream: &crate::transport::imp::Stream) -> io::Result<Option<Peer>> {
        match *stream {}
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn identifies_own_process() {
        let (a, _b) = Stream::pair().unwrap();
        let peer = Peer::identify_owner(&a).unwrap();
        assert_eq!(peer.pid, Some(std::process::id() as i32));
        assert_eq!(peer.exe, Some(std::env::current_exe().unwrap()));
        assert!(peer._pin.is_some());
        // Tests run from a local login (or none), never a remote one.
        assert!(!peer.remote_session);
        // Some ancestor of the test runner (cargo, a shell, a terminal) is the application.
        assert!(peer.app.is_some());
    }

    #[test]
    fn parses_tty_from_stat() {
        let stat = "123 (a b) c)) S 1 123 123 34816 123 0 0";
        assert_eq!(imp::controlling_tty(stat), Some(34816));
        assert_eq!(imp::controlling_tty("1 (init) S 0 1 1 0 -1"), None);
    }
}
