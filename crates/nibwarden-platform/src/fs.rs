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

#[cfg(target_os = "linux")]
mod imp {
    use std::{
        fs::{self, File},
        io,
        os::unix::fs::{
            DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
        },
        path::Path,
    };

    pub fn create_private_dir(dir: &Path) -> io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let meta = fs::metadata(dir)?;
        // SAFETY: geteuid has no failure mode.
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is owned by another user", dir.display()),
            ));
        }
        if meta.mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    pub fn restrict_file(path: &Path) -> io::Result<()> {
        match fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// The file and every directory above it belong to root and are not writable by group
    /// or others.
    pub fn is_system_path(path: &Path) -> bool {
        path.is_absolute()
            && path
                .ancestors()
                .all(|p| fs::metadata(p).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0))
    }

    pub fn try_lock(path: &Path) -> io::Result<Option<File>> {
        use std::os::fd::AsRawFd as _;

        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(path)?;
        // SAFETY: flock on a descriptor we own; non-blocking.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        }
    }

    pub fn create_private_file(path: &Path) -> io::Result<File> {
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::{fs::File, io, path::Path};

    use crate::unsupported;

    pub fn create_private_dir(_: &Path) -> io::Result<()> {
        Err(unsupported())
    }

    pub fn restrict_file(_: &Path) -> io::Result<()> {
        Err(unsupported())
    }

    pub fn create_private_file(_: &Path) -> io::Result<File> {
        Err(unsupported())
    }

    pub fn is_system_path(_: &Path) -> bool {
        false
    }

    pub fn try_lock(_: &Path) -> io::Result<Option<File>> {
        Err(unsupported())
    }
}

#[cfg(all(test, target_os = "linux"))]
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
