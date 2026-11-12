use std::{
    io,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::unsupported;

/// No value of these types can exist until the platform is ported.
pub enum Listener {}
pub enum Stream {}

pub async fn bind(_: &Path) -> io::Result<Listener> {
    Err(unsupported())
}

pub async fn accept(listener: &Listener) -> io::Result<Stream> {
    match *listener {}
}

pub async fn connect(_: &Path) -> io::Result<Stream> {
    Err(unsupported())
}

pub fn pair() -> io::Result<(Stream, Stream)> {
    Err(unsupported())
}

pub fn remove(_: &Path) {}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match *self {}
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        match *self {}
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match *self {}
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match *self {}
    }
}
