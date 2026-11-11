//! Local IPC endpoints private to the current user: the control socket and the SSH agent
//! (Linux: Unix sockets; Windows will use named pipes).

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

#[cfg_attr(target_os = "linux", path = "linux/transport.rs")]
#[cfg_attr(target_os = "macos", path = "macos/transport.rs")]
#[cfg_attr(windows, path = "windows/transport.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/transport.rs"
)]
pub(crate) mod imp;

#[cfg(all(test, target_os = "linux"))]
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
