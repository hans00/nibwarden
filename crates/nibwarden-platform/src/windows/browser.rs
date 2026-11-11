// Not ported yet: reports Unsupported like other platforms.
use std::io;

pub fn open(_: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "opening websites is not supported on this platform yet",
    ))
}
