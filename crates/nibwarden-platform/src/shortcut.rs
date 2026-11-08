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

#[cfg(target_os = "linux")]
mod imp {
    use std::{io, process::Command};

    use super::Installed;

    const SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
    const PATH: &str =
        "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/nibwarden-quick/";

    /// GNOME: a custom keybinding through gsettings. Other desktops: not automatic yet.
    pub fn install(name: &str, command: &str, binding: &str) -> io::Result<Installed> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        if !desktop.split(':').any(|d| d.eq_ignore_ascii_case("gnome")) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "cannot add shortcuts on this desktop ({}) yet; add one in its keyboard \
                     settings that runs: {command}",
                    if desktop.is_empty() {
                        "unknown"
                    } else {
                        &desktop
                    }
                ),
            ));
        }
        let current = gsettings(&["get", SCHEMA, "custom-keybindings"])?;
        let mut paths = parse_string_list(&current);
        if !paths.iter().any(|p| p == PATH) {
            paths.push(PATH.to_owned());
            gsettings(&[
                "set",
                SCHEMA,
                "custom-keybindings",
                &format_string_list(&paths),
            ])?;
        }
        let entry = format!("{SCHEMA}.custom-keybinding:{PATH}");
        gsettings(&["set", &entry, "name", name])?;
        gsettings(&["set", &entry, "command", command])?;
        gsettings(&["set", &entry, "binding", binding])?;
        Ok(Installed {
            location: format!("GNOME Settings → Keyboard → Custom Shortcuts ({name})"),
        })
    }

    fn gsettings(args: &[&str]) -> io::Result<String> {
        let out = Command::new("gsettings").args(args).output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "gsettings {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// `@as []` or `['a', 'b']` (GVariant text format of a string array) → items.
    pub(super) fn parse_string_list(text: &str) -> Vec<String> {
        let inner = text
            .trim()
            .trim_start_matches("@as")
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']');
        inner
            .split(',')
            .map(|item| {
                item.trim()
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_owned()
            })
            .filter(|item| !item.is_empty())
            .collect()
    }

    pub(super) fn format_string_list(items: &[String]) -> String {
        let quoted: Vec<String> = items.iter().map(|i| format!("'{i}'")).collect();
        format!("[{}]", quoted.join(", "))
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::Installed;

    pub fn install(_: &str, _: &str, _: &str) -> std::io::Result<Installed> {
        Err(crate::unsupported())
    }
}

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
