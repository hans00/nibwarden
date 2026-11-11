// Not ported yet: reports Unsupported like other platforms.
pub fn available() -> bool {
    false
}

/// Elsewhere, the renderer is trusted to fail when it is chosen.
pub fn opengl_available() -> bool {
    true
}
