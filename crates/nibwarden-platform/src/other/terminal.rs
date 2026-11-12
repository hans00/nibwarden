use std::{fs::File, io};

pub fn open() -> io::Result<File> {
    Err(crate::unsupported())
}
