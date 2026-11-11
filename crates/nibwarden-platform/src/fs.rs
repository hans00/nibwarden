//! Files and directories only the current user may read.

use std::{fs::File, io, path::Path};

/// Creates `dir` (and parents) private to the current user, or checks an existing one:
/// it must belong to us, and group/other permissions are removed.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    imp::create_private_dir(dir)
}

/// Restricts an existing file to the current user. A missing file is not an error.
pub fn restrict_file(path: &Path) -> io::Result<()> {
    imp::restrict_file(path)
}

/// Creates or truncates `path` for writing, private to the current user from the moment
/// it exists.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    imp::create_private_file(path)
}

/// Whether `path` is a program installed by the system, which the current user cannot
/// replace: approvals are only remembered for such programs (phase2.md §4.2), since a
/// remembered approval for a user-writable file would pass to whatever is put there.
pub fn is_system_path(path: &Path) -> bool {
    imp::is_system_path(path)
}

/// Takes an exclusive lock on `path` (created if needed) without waiting; `None` if another
/// process holds it. The lock lasts as long as the returned file is open.
pub fn try_lock(path: &Path) -> io::Result<Option<File>> {
    imp::try_lock(path)
}

#[cfg_attr(any(target_os = "linux", target_os = "macos"), path = "unix/fs.rs")]
#[cfg_attr(windows, path = "windows/fs.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/fs.rs"
)]
mod imp;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn system_paths() {
        assert!(is_system_path(Path::new("/usr/bin/env")));
        assert!(!is_system_path(&std::env::current_exe().unwrap()));
        assert!(!is_system_path(Path::new("/tmp")));
        assert!(!is_system_path(Path::new("relative")));
    }

    #[test]
    fn lock_is_exclusive_until_dropped() {
        let path = std::env::temp_dir().join(format!("nw-lock-{}", std::process::id()));
        let held = try_lock(&path).unwrap();
        assert!(held.is_some());
        assert!(try_lock(&path).unwrap().is_none());
        drop(held);
        assert!(try_lock(&path).unwrap().is_some());
        std::fs::remove_file(path).unwrap();
    }
}
