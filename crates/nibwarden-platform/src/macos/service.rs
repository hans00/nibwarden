// Not ported yet: reports Unsupported like other platforms.
pub async fn start_daemon() -> bool {
    false
}

pub fn set_start_at_login(_: bool) -> std::io::Result<()> {
    Err(crate::unsupported())
}

pub fn starts_at_login() -> bool {
    false
}
