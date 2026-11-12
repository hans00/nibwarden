use std::io;

use super::Installed;

pub fn install(_: &str, _: &str, _: &str) -> io::Result<Installed> {
    Err(crate::unsupported())
}

pub fn current() -> Option<String> {
    None
}

pub fn remove() -> io::Result<()> {
    Ok(())
}

pub fn supported() -> bool {
    false
}

pub const NEEDS_HOLDER: bool = false;

#[cfg(feature = "hotkey")]
pub enum Held {}

#[cfg(feature = "hotkey")]
pub fn hold(_: &str, _: Box<dyn Fn() + Send + Sync>) -> io::Result<Held> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the desktop runs the shortcut itself here",
    ))
}
