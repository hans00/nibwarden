//! Where nibwarden keeps its files.

use std::{io, path::PathBuf};

/// Directory (private to the user) for the daemon's sockets; `None` if the platform's
/// per-session runtime directory is unknown.
pub fn runtime_dir() -> Option<PathBuf> {
    imp::runtime_dir()
}

/// Session, encrypted vault cache and other persistent state.
pub fn data_dir() -> io::Result<PathBuf> {
    imp::data_dir()
}

/// User settings (`config.toml`).
pub fn config_dir() -> io::Result<PathBuf> {
    imp::config_dir()
}

#[cfg(target_os = "linux")]
mod imp {
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
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::{io, path::PathBuf};

    pub fn runtime_dir() -> Option<PathBuf> {
        None
    }

    pub fn data_dir() -> io::Result<PathBuf> {
        Err(crate::unsupported())
    }

    pub fn config_dir() -> io::Result<PathBuf> {
        Err(crate::unsupported())
    }
}
