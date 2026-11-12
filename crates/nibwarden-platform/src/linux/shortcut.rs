use std::{io, process::Command};

use super::Installed;

const SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/nibwarden-quick/";

fn desktop() -> String {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()
}

pub fn supported() -> bool {
    desktop()
        .split(':')
        .any(|d| d.eq_ignore_ascii_case("gnome"))
}

fn entry() -> String {
    format!("{SCHEMA}.custom-keybinding:{PATH}")
}

pub fn current() -> Option<String> {
    if !supported() {
        return None;
    }
    let paths = parse_string_list(&gsettings(&["get", SCHEMA, "custom-keybindings"]).ok()?);
    if !paths.iter().any(|p| p == PATH) {
        return None;
    }
    let binding = gsettings(&["get", &entry(), "binding"]).ok()?;
    let binding = binding.trim_matches('\'').to_owned();
    (!binding.is_empty()).then_some(binding)
}

pub fn remove() -> io::Result<()> {
    if !supported() {
        return Ok(());
    }
    let mut paths = parse_string_list(&gsettings(&["get", SCHEMA, "custom-keybindings"])?);
    if paths.iter().any(|p| p == PATH) {
        paths.retain(|p| p != PATH);
        gsettings(&[
            "set",
            SCHEMA,
            "custom-keybindings",
            &format_string_list(&paths),
        ])?;
        gsettings(&["reset-recursively", &entry()])?;
    }
    Ok(())
}

/// GNOME: a custom keybinding through gsettings. Other desktops: not automatic yet.
pub fn install(name: &str, command: &str, binding: &str) -> io::Result<Installed> {
    let desktop = desktop();
    if !supported() {
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
    let entry = entry();
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

pub const NEEDS_HOLDER: bool = false;

#[cfg(feature = "hotkey")]
pub enum Held {}

#[cfg(feature = "hotkey")]
pub fn hold(_: &str, _: Box<dyn Fn() + Send + Sync>) -> io::Result<Held> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the desktop runs the shortcut itself here",
    ))
}
