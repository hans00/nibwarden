use std::{io, path::PathBuf};

use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData};

use crate::winutil::known_folder;

/// `%LOCALAPPDATA%\nibwarden\run`: lock files and the like. The endpoints the daemon
/// listens on are named pipes and have no file here; see `transport`. Under the user's
/// profile, hence private to the user.
pub fn runtime_dir() -> Option<PathBuf> {
    data_dir().ok().map(|dir| dir.join("run"))
}

/// `%LOCALAPPDATA%\nibwarden`: local, not roaming, since the vault cache and the session
/// belong to this machine.
pub fn data_dir() -> io::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_LocalAppData)?.join("nibwarden"))
}

/// `%APPDATA%\nibwarden`.
pub fn config_dir() -> io::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_RoamingAppData)?.join("nibwarden"))
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn under_the_users_profile() {
        let data = data_dir().unwrap();
        let config = config_dir().unwrap();
        assert!(data.ends_with("nibwarden") && config.ends_with("nibwarden"));
        assert_ne!(data, config);
        assert_eq!(runtime_dir().unwrap(), data.join("run"));
        assert!(data.is_absolute() && config.is_absolute());
    }
}
