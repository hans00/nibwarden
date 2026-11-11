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
    /// Start time of the process (Linux: clock ticks since boot; macOS: microseconds
    /// since the epoch), so a new process that reuses the pid is a different application.
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

#[cfg_attr(target_os = "linux", path = "linux/peer.rs")]
#[cfg_attr(target_os = "macos", path = "macos/peer.rs")]
#[cfg_attr(windows, path = "windows/peer.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/peer.rs"
)]
mod imp;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn identifies_own_process() {
        let (a, _b) = Stream::pair().unwrap();
        let peer = Peer::identify_owner(&a).unwrap();
        assert_eq!(peer.pid, Some(std::process::id() as i32));
        let canonical = |p: PathBuf| p.canonicalize().unwrap();
        assert_eq!(
            peer.exe.map(canonical),
            Some(canonical(std::env::current_exe().unwrap()))
        );
        assert!(peer._pin.is_some());
        // Tests run from a local login (or none), never a remote one.
        assert!(!peer.remote_session);
        // Some ancestor of the test runner (cargo, a shell, a terminal) is the application.
        assert!(peer.app.is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_tty_from_stat() {
        let stat = "123 (a b) c)) S 1 123 123 34816 123 0 0";
        assert_eq!(imp::controlling_tty(stat), Some(34816));
        assert_eq!(imp::controlling_tty("1 (init) S 0 1 1 0 -1"), None);
    }
}
