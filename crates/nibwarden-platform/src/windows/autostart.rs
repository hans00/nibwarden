// Not ported yet: reports Unsupported like other platforms.
use std::io;

pub fn set(_: &str, _: &str, _: &[String], _: bool) -> io::Result<()> {
    Err(crate::unsupported())
}

pub fn is_set(_: &str) -> bool {
    false
}
