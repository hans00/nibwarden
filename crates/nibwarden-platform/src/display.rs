//! Whether this session can show windows (phase2.md §6).

/// True if a window opened now would appear on the user's desktop.
pub fn available() -> bool {
    imp::available()
}

/// True if an OpenGL driver answers, so windows can be drawn with it. Asked before
/// choosing a renderer: one that fails only when its window opens leaves no window at all.
pub fn opengl_available() -> bool {
    imp::opengl_available()
}

#[cfg_attr(target_os = "linux", path = "linux/display.rs")]
#[cfg_attr(target_os = "macos", path = "macos/display.rs")]
#[cfg_attr(windows, path = "windows/display.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/display.rs"
)]
mod imp;
