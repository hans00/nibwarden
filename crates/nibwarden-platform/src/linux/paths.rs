use std::{io, path::PathBuf};

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `$<var>/nibwarden`, or `~/<fallback>/nibwarden` (XDG base directories).
fn xdg_dir(var: &str, fallback: &str) -> io::Result<PathBuf> {
    if let Some(dir) = env_dir(var) {
        return Ok(dir.join("nibwarden"));
    }
    let home = env_dir("HOME").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("neither {var} nor HOME is set"),
        )
    })?;
    Ok(home.join(fallback).join("nibwarden"))
}

/// `$XDG_RUNTIME_DIR/nibwarden`.
pub fn runtime_dir() -> Option<PathBuf> {
    env_dir("XDG_RUNTIME_DIR").map(|dir| dir.join("nibwarden"))
}

/// `$XDG_DATA_HOME/nibwarden`, by default `~/.local/share/nibwarden`.
pub fn data_dir() -> io::Result<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_CONFIG_HOME/nibwarden`, by default `~/.config/nibwarden`.
pub fn config_dir() -> io::Result<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}
