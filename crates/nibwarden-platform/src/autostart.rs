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

#[cfg(target_os = "linux")]
mod imp {
    use std::{io, path::PathBuf};

    /// `$XDG_CONFIG_HOME/autostart/<name>.desktop` (XDG Autostart).
    fn entry(name: &str) -> io::Result<PathBuf> {
        let config = crate::paths::config_dir()?;
        let home = config
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config directory"))?;
        Ok(home.join("autostart").join(format!("{name}.desktop")))
    }

    pub fn set(name: &str, title: &str, command: &[String], on: bool) -> io::Result<()> {
        let path = entry(name)?;
        if !on {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, desktop_entry(title, command))
    }

    pub fn is_set(name: &str) -> bool {
        entry(name).is_ok_and(|path| path.exists())
    }

    pub(super) fn desktop_entry(title: &str, command: &[String]) -> String {
        let exec: Vec<String> = command.iter().map(|arg| quote(arg)).collect();
        format!(
            "[Desktop Entry]\nType=Application\nName={title}\nExec={}\nNoDisplay=true\n\
             X-GNOME-Autostart-enabled=true\n",
            exec.join(" ")
        )
    }

    /// An `Exec` argument (Desktop Entry Specification): quoted when it has to be, with
    /// `"`, `` ` ``, `$` and `\` escaped inside the quotes and `%` doubled; then every
    /// backslash doubled, since string values are unescaped before the quoting applies.
    fn quote(arg: &str) -> String {
        let arg = arg.replace('%', "%%");
        let plain = !arg.is_empty()
            && !arg.chars().any(|c| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '"' | '\''
                            | '\\'
                            | '>'
                            | '<'
                            | '~'
                            | '|'
                            | '&'
                            | ';'
                            | '$'
                            | '*'
                            | '?'
                            | '#'
                            | '('
                            | ')'
                            | '`'
                            | '='
                    )
            });
        if plain {
            return arg;
        }
        let mut quoted = String::from("\"");
        for c in arg.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
        quoted.replace('\\', "\\\\")
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::io;

    pub fn set(_: &str, _: &str, _: &[String], _: bool) -> io::Result<()> {
        Err(crate::unsupported())
    }

    pub fn is_set(_: &str) -> bool {
        false
    }
}

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
