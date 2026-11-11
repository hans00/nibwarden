use super::Installed;

pub fn install(_: &str, _: &str, _: &str) -> std::io::Result<Installed> {
    Err(crate::unsupported())
}

pub fn current() -> Option<String> {
    None
}

pub fn remove() -> std::io::Result<()> {
    Ok(())
}

pub fn supported() -> bool {
    false
}
