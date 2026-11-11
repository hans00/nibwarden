//! Named pipes.
//!
//! The `Path` of the interface is mapped to a pipe name (see [`endpoint`]): the pipe
//! namespace is shared by all users of the machine, so the user's SID is part of the name.
//! A path that already is a pipe name (`\\.\pipe\...`) is used as it is.
//!
//! A server pipe is created with a security descriptor that admits only the current user
//! (the default one would let Everyone and Anonymous read), refuses remote clients, and
//! the first instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`: if the name is taken, by a live
//! daemon or by a squatter, creating it fails and that is `AddrInUse`. A client checks that
//! the process serving the pipe runs as the current user before it says anything, and
//! opens it so that the server may identify but not impersonate it.

use std::{
    ffi::{OsStr, OsString},
    io,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle},
    path::{Path, PathBuf},
    pin::Pin,
    sync::atomic::{AtomicU32, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
    },
};
use windows::Win32::{
    Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, HANDLE},
    System::{
        Pipes::GetNamedPipeServerProcessId,
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    },
};

use crate::winutil::{IoResult as _, OwnerOnly, current_user, process_user};

const PIPE_PREFIX: &str = r"\\.\pipe\";

/// `SECURITY_IDENTIFICATION`: the server may learn who connected, not act as them.
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

pub enum Stream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl Stream {
    pub(crate) fn raw_handle(&self) -> RawHandle {
        match self {
            Self::Server(s) => s.as_raw_handle(),
            Self::Client(c) => c.as_raw_handle(),
        }
    }
}

macro_rules! delegate {
    ($self:ident, $s:ident => $call:expr) => {
        match $self.get_mut() {
            Stream::Server($s) => $call,
            Stream::Client($s) => $call,
        }
    };
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        delegate!(self, s => Pin::new(s).poll_read(cx, buf))
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        delegate!(self, s => Pin::new(s).poll_write(cx, buf))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, s => Pin::new(s).poll_flush(cx))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, s => Pin::new(s).poll_shutdown(cx))
    }
}

/// The instance waiting for the next client, and the name for the ones after it.
pub struct Listener {
    name: OsString,
    next: tokio::sync::Mutex<Option<NamedPipeServer>>,
}

/// The pipe name for the endpoint bound at `path`, as other programs (`ssh`, which is told
/// in `SSH_AUTH_SOCK` or `IdentityAgent`) have to spell it.
///
/// - `\\.\pipe\name` is taken as it is.
/// - Otherwise `\\.\pipe\nibwarden-<user SID>-<file stem>`, and for a path outside the
///   runtime directory a hash of its directory too, so that two endpoints of the same name
///   in different places (tests, `--socket`) stay apart. The default
///   `<runtime dir>\daemon.sock` thus becomes `\\.\pipe\nibwarden-S-1-5-21-...-daemon`.
pub fn endpoint(path: &Path) -> PathBuf {
    pipe_name(path).map_or_else(|_| path.to_owned(), PathBuf::from)
}

fn pipe_name(path: &Path) -> io::Result<OsString> {
    let text = path.as_os_str().to_string_lossy();
    if text.len() >= PIPE_PREFIX.len()
        && text[..PIPE_PREFIX.len()].eq_ignore_ascii_case(PIPE_PREFIX)
    {
        return Ok(path.as_os_str().to_owned());
    }
    let stem = path
        .file_stem()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "endpoint path has no name"))?
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let sid = current_user()?.to_sddl()?;
    let mut name = format!("{PIPE_PREFIX}nibwarden-{sid}-{stem}");
    let parent = path.parent().map(Path::to_owned).unwrap_or_default();
    if crate::paths::runtime_dir().as_deref() != Some(parent.as_path()) {
        let dir = parent.to_string_lossy().to_lowercase();
        // FNV-1a: only to tell directories apart.
        let hash = dir.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        name.push_str(&format!("-{:08x}", hash & 0xffff_ffff));
    }
    Ok(name.into())
}

/// A new instance of the pipe `name`, for the current user alone.
fn create_instance(name: &OsStr, first: bool) -> io::Result<NamedPipeServer> {
    let mut security = OwnerOnly::new("FA", false)?;
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte);
    // SAFETY: the attributes point into `security`, which outlives the call; the pipe
    // keeps its own copy of the descriptor.
    unsafe { options.create_with_security_attributes_raw(name, security.attributes_mut()) }
}

impl Listener {
    /// The handle of the instance waiting for a client (tests inspect its descriptor).
    #[cfg(test)]
    pub(crate) async fn next_handle_for_tests(&self) -> RawHandle {
        let next = self.next.lock().await;
        next.as_ref()
            .expect("an instance is waiting")
            .as_raw_handle()
    }
}

pub async fn bind(path: &Path) -> io::Result<Listener> {
    let name = pipe_name(path)?;
    // Where the endpoint is a file path, its directory is kept private like on Unix.
    if name != path.as_os_str()
        && let Some(dir) = path.parent()
    {
        crate::fs::create_private_dir(dir)?;
    }
    let first = create_instance(&name, true).map_err(|e| {
        // With FILE_FLAG_FIRST_PIPE_INSTANCE, a name that exists (a live daemon, or someone
        // else squatting on it) is refused with ERROR_ACCESS_DENIED.
        let taken = e.raw_os_error().is_some_and(|code| {
            code == ERROR_ACCESS_DENIED.0 as i32 || code == ERROR_PIPE_BUSY.0 as i32
        });
        if taken {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "another process is already listening on {}",
                    name.to_string_lossy()
                ),
            )
        } else {
            e
        }
    })?;
    Ok(Listener {
        name,
        next: tokio::sync::Mutex::new(Some(first)),
    })
}

pub async fn accept(listener: &Listener) -> io::Result<Stream> {
    let mut next = listener.next.lock().await;
    let waiting = match next.take() {
        Some(server) => server,
        // The instance after the previous client could not be made then.
        None => create_instance(&listener.name, false)?,
    };
    waiting.connect().await?;
    // The next instance exists before this client is handed out, so there is no moment
    // when the name has no waiting instance and a client would find it busy.
    *next = Some(create_instance(&listener.name, false)?);
    Ok(Stream::Server(waiting))
}

pub async fn connect(path: &Path) -> io::Result<Stream> {
    let name = pipe_name(path)?;
    let mut tries = 0;
    let client = loop {
        let mut options = ClientOptions::new();
        options.security_qos_flags(SECURITY_IDENTIFICATION);
        match options.open(&name) {
            Ok(client) => break client,
            // All instances are in use for a moment.
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) && tries < 40 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e),
        }
    };
    server_is_ours(&client)?;
    Ok(Stream::Client(client))
}

/// Whoever created the pipe could be anyone who got to the name first; only a server
/// running as us may hear what a client has to say.
fn server_is_ours(client: &NamedPipeClient) -> io::Result<()> {
    let mut pid = 0u32;
    // SAFETY: the handle is the open client end; `pid` is written.
    unsafe { GetNamedPipeServerProcessId(HANDLE(client.as_raw_handle()), &mut pid)? };
    // SAFETY: the new handle is owned from here on.
    let process = unsafe {
        OwnedHandle::from_raw_handle(
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .io()?
                .0,
        )
    };
    let user = process_user(HANDLE(process.as_raw_handle()))?;
    if user == current_user()? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the pipe is served by a process of another user",
        ))
    }
}

/// A connected pair, for tests. Needs a Tokio runtime, like the Unix one.
pub fn pair() -> io::Result<(Stream, Stream)> {
    use futures_util::FutureExt as _;

    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let name = OsString::from(format!(
        "{PIPE_PREFIX}nibwarden-pair-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let server = create_instance(&name, true)?;
    let mut options = ClientOptions::new();
    options.security_qos_flags(SECURITY_IDENTIFICATION);
    let client = options.open(&name)?;
    // The client has opened the pipe, so connecting the server end completes at once.
    server
        .connect()
        .now_or_never()
        .ok_or_else(|| io::Error::other("pipe did not connect"))??;
    Ok((Stream::Client(client), Stream::Server(server)))
}

pub fn remove(_: &Path) {}
