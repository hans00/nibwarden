//! Programs started when the user logs in to the desktop (phase2.md P2-M8): the tray icon.

use std::io;

/// Starts `command` (program and arguments) at every login from now on, under `name`
/// (a file-name-safe identifier), or stops doing so. Turning off what is not on is fine.
pub fn set(name: &str, title: &str, command: &[String], on: bool) -> io::Result<()> {
    imp::set(name, title, command, on)
}

/// Whether `name` starts at login.
pub fn is_set(name: &str) -> bool {
    imp::is_set(name)
}

#[cfg_attr(target_os = "linux", path = "linux/autostart.rs")]
#[cfg_attr(target_os = "macos", path = "macos/autostart.rs")]
#[cfg_attr(windows, path = "windows/autostart.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/autostart.rs"
)]
mod imp;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::imp::desktop_entry;

    #[test]
    fn writes_an_exec_line() {
        let command = [
            "/opt/nib warden/nibwarden-gui".to_owned(),
            "tray".to_owned(),
        ];
        let entry = desktop_entry("nibwarden", &command);
        assert!(entry.contains("\nExec=\"/opt/nib warden/nibwarden-gui\" tray\n"));
        let odd = ["/a$b/100%".to_owned()];
        assert!(desktop_entry("x", &odd).contains("Exec=\"/a\\\\$b/100%%\"\n"));
    }
}
