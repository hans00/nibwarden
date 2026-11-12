/// The process belongs to a graphical login session (`CGSessionCopyCurrentDictionary`),
/// which a process started over SSH, or outside the user's login, does not.
pub fn available() -> bool {
    crate::gui_session::exists()
}

/// Always: macOS draws OpenGL through Metal on every supported Mac (Apple deprecated it,
/// but it is not removed), so there is no driver to be missing as on Linux.
pub fn opengl_available() -> bool {
    true
}
