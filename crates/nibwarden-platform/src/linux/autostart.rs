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
