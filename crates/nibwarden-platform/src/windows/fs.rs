// Not ported yet: reports Unsupported like other platforms.
use std::{fs::File, io, path::Path};

use crate::unsupported;

pub fn create_private_dir(_: &Path) -> io::Result<()> {
    Err(unsupported())
}

pub fn restrict_file(_: &Path) -> io::Result<()> {
    Err(unsupported())
}

pub fn create_private_file(_: &Path) -> io::Result<File> {
    Err(unsupported())
}

pub fn is_system_path(_: &Path) -> bool {
    false
}

pub fn try_lock(_: &Path) -> io::Result<Option<File>> {
    Err(unsupported())
}
