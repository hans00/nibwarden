use std::{fs, io, os::unix::fs::PermissionsExt as _, path::Path};

pub use tokio::net::{UnixListener as Listener, UnixStream as Stream};

pub async fn bind(path: &Path) -> io::Result<Listener> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"))?;
    crate::fs::create_private_dir(dir)?;
    if path.exists() {
        if Stream::connect(path).await.is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("another process is already listening on {}", path.display()),
            ));
        }
        fs::remove_file(path)?;
    }
    let listener = Listener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn accept(listener: &Listener) -> io::Result<Stream> {
    Ok(listener.accept().await?.0)
}

pub async fn connect(path: &Path) -> io::Result<Stream> {
    Stream::connect(path).await
}

pub fn pair() -> io::Result<(Stream, Stream)> {
    Stream::pair()
}

pub fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}
