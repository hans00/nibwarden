use std::io;

pub async fn available() -> bool {
    false
}

pub async fn authenticate(_: u32) -> io::Result<bool> {
    Err(crate::unsupported())
}
