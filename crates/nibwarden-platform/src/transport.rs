//! Local IPC endpoints private to the current user: the control socket and the SSH agent
//! (Unix sockets; on Windows, named pipes).

use std::{
    io,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A listening endpoint. Dropping it removes the endpoint.
pub struct Listener {
    inner: imp::Listener,
    path: PathBuf,
}

impl Listener {
    /// Binds `path` in a directory private to the user. A stale endpoint left by a dead
    /// process is replaced; a live one fails with [`io::ErrorKind::AddrInUse`].
    pub async fn bind(path: &Path) -> io::Result<Self> {
        Ok(Self {
            inner: imp::bind(path).await?,
            path: path.to_owned(),
        })
    }

    pub async fn accept(&self) -> io::Result<Stream> {
        Ok(Stream(imp::accept(&self.inner).await?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        imp::remove(&self.path);
    }
}

/// The name other programs (an `ssh` told through `SSH_AUTH_SOCK`) use to reach the endpoint
/// bound at `path`. A Unix socket is its path; on Windows the path is mapped to a named
/// pipe, and this is `\\.\pipe\...`.
pub fn endpoint(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        imp::endpoint(path)
    }
    #[cfg(not(windows))]
    {
        path.to_owned()
    }
}

/// One connection, either end.
pub struct Stream(pub(crate) imp::Stream);

impl Stream {
    pub async fn connect(path: &Path) -> io::Result<Self> {
        Ok(Self(imp::connect(path).await?))
    }

    /// Two connected ends, for tests.
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = imp::pair()?;
        Ok((Self(a), Self(b)))
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

#[cfg_attr(
    any(target_os = "linux", target_os = "macos"),
    path = "unix/transport.rs"
)]
#[cfg_attr(windows, path = "windows/transport.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/transport.rs"
)]
pub(crate) mod imp;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn bind_replaces_stale_refuses_live_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("nw-transport-{}", std::process::id()));
        let path = dir.join("run/t.sock");
        let listener = Listener::bind(&path).await.unwrap();
        let err = Listener::bind(&path).await.err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        // That check connected to find the listener alive; its connection is queued first.
        drop(listener.accept().await.unwrap());

        let mut client = Stream::connect(&path).await.unwrap();
        let mut server = listener.accept().await.unwrap();
        client.write_all(b"hi").await.unwrap();
        let mut buf = [0; 2];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi");

        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);

        drop(listener);
        assert!(!path.exists());
        // A socket file nobody listens on is stale and gets replaced.
        let stale = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(stale);
        let _again = Listener::bind(&path).await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use windows::Win32::{
        Foundation::{HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo,
                SDDL_REVISION_1, SE_KERNEL_OBJECT,
            },
            DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
    };

    use super::*;

    fn unique_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("nw-transport-{}-{name}", std::process::id()))
            .join("t.sock")
    }

    #[tokio::test]
    async fn bind_refuses_live_round_trips_and_names_the_pipe() {
        let path = unique_path("round");
        let listener = Listener::bind(&path).await.unwrap();
        // A second listener on the name is refused, not stacked as another instance.
        // (Wine ignores FILE_FLAG_FIRST_PIPE_INSTANCE and allows it.)
        let second = Listener::bind(&path).await;
        if crate::winutil::under_wine() {
            drop(second);
        } else {
            assert_eq!(second.err().unwrap().kind(), io::ErrorKind::AddrInUse);
        }

        let name = endpoint(&path);
        assert!(
            name.to_string_lossy()
                .starts_with(r"\\.\pipe\nibwarden-S-1-")
        );
        assert_eq!(endpoint(&name), name);

        let mut client = Stream::connect(&path).await.unwrap();
        let mut server = listener.accept().await.unwrap();
        client.write_all(b"hi").await.unwrap();
        let mut buf = [0; 2];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi");
        server.write_all(b"yo").await.unwrap();
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"yo");

        // A second client is served while the first is connected.
        let _second = Stream::connect(&path).await.unwrap();
        let _server2 = listener.accept().await.unwrap();

        drop((client, server, _second, _server2));
        drop(listener);
        // Gone with its last instance: nobody listens, and the name can be taken again.
        // (A closed instance lingers until the runtime has seen its cancelled operations.)
        let mut tries = 0;
        let _again = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tries += 1;
            assert!(tries < 100, "the pipe never went away");
            if Stream::connect(&path).await.is_ok() {
                continue;
            }
            match Listener::bind(&path).await {
                Ok(listener) => break listener,
                Err(e) => assert_eq!(e.kind(), io::ErrorKind::AddrInUse),
            }
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn only_the_current_user_may_open_the_pipe() {
        let path = unique_path("acl");
        let listener = Listener::bind(&path).await.unwrap();
        let instance = listener.inner.next_handle_for_tests().await;
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let mut text = windows::core::PWSTR::null();
        // SAFETY: outputs are valid; the descriptor and the string are freed below.
        let sddl = unsafe {
            GetSecurityInfo(
                HANDLE(instance),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&mut descriptor),
            )
            .ok()
            .unwrap();
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                None,
            )
            .unwrap();
            let sddl = String::from_utf16_lossy(text.as_wide());
            let _ = LocalFree(Some(HLOCAL(text.0.cast())));
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            sddl
        };
        let sid = crate::winutil::current_user().unwrap().to_sddl().unwrap();
        // Protected, one entry, for our SID: no Everyone, Anonymous or network.
        assert!(sddl.starts_with("D:P(A;"), "{sddl}");
        assert!(sddl.ends_with(&format!(";;;{sid})")), "{sddl}");
        assert_eq!(sddl.matches("(A;").count(), 1, "{sddl}");
        assert!(!sddl.contains("(D;") && !sddl.contains(";;;WD)"), "{sddl}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn pairs_talk() {
        let (mut a, mut b) = Stream::pair().unwrap();
        a.write_all(b"ping").await.unwrap();
        let mut buf = [0; 4];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}
