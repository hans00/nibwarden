//! A desktop-wide keyboard shortcut that runs a command (phase2.md §7): the way to call
//! up quick access. Wayland lets no ordinary program grab keys itself, so the shortcut is
//! added to the desktop's own settings.

use std::io;

/// The shortcut as installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// Where it was written, for telling the user.
    pub location: String,
}

/// Binds `binding` (GTK accelerator syntax, e.g. `<Control><Shift>space`) to `command`.
/// Installing again replaces the previous nibwarden shortcut.
pub fn install(name: &str, command: &str, binding: &str) -> io::Result<Installed> {
    imp::install(name, command, binding)
}

/// The binding of the shortcut [`install`] added, if it is there.
pub fn current() -> Option<String> {
    imp::current()
}

/// Removes the shortcut [`install`] added.
pub fn remove() -> io::Result<()> {
    imp::remove()
}

/// Whether this desktop's shortcuts can be set from nibwarden.
pub fn supported() -> bool {
    imp::supported()
}

#[cfg_attr(target_os = "linux", path = "linux/shortcut.rs")]
#[cfg_attr(target_os = "macos", path = "macos/shortcut.rs")]
#[cfg_attr(windows, path = "windows/shortcut.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/shortcut.rs"
)]
mod imp;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::imp::{format_string_list, parse_string_list};

    #[test]
    fn gvariant_string_lists() {
        assert!(parse_string_list("@as []").is_empty());
        let list = parse_string_list("['/a/', '/b/']");
        assert_eq!(list, ["/a/", "/b/"]);
        assert_eq!(format_string_list(&list), "['/a/', '/b/']");
    }
}
