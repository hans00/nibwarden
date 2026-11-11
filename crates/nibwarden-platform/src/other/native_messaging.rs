use std::{io, path::Path};

use super::Browser;

pub fn browsers(_: &Path) -> Vec<Browser> {
    Vec::new()
}

pub fn register(_: &Path, _: bool) -> io::Result<Vec<Browser>> {
    Err(crate::unsupported())
}

pub fn refresh(_: &Path) -> io::Result<()> {
    Ok(())
}

pub fn unregister(_: &Path) -> io::Result<()> {
    Err(crate::unsupported())
}
