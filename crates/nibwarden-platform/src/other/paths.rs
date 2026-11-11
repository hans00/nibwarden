use std::{io, path::PathBuf};

pub fn runtime_dir() -> Option<PathBuf> {
    None
}

pub fn data_dir() -> io::Result<PathBuf> {
    Err(crate::unsupported())
}

pub fn config_dir() -> io::Result<PathBuf> {
    Err(crate::unsupported())
}
