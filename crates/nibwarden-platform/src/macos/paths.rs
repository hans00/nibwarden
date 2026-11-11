use std::{
    io,
    path::{Path, PathBuf},
};

/// Longest runtime directory we accept: `sun_path` holds 104 bytes including the NUL,
/// and the longest socket name in the directory is a few dozen bytes.
const MAX_RUNTIME_DIR: usize = 60;

/// The user's home directory: `$HOME`, which launchd and login shells set, else the
/// account database.
fn home() -> io::Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    passwd_home().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))
}

fn passwd_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;

    let mut buf = vec![0u8; 4096];
    // SAFETY: an all-zero passwd is a valid out-parameter for getpwuid_r.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: getpwuid_r writes into `pwd` and `buf`, which outlive the call; on success
    // `pw_dir` points into `buf`, and is copied before `buf` is dropped.
    unsafe {
        let rc = libc::getpwuid_r(
            libc::geteuid(),
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        );
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr(pwd.pw_dir);
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
    }
}

/// `confstr(_CS_DARWIN_USER_TEMP_DIR)`: the user's private temporary directory
/// (`/var/folders/xx/.../T`), independent of `$TMPDIR`.
fn user_temp_dir() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;

    let mut buf = vec![0u8; 1024];
    // SAFETY: confstr writes at most `len` bytes, NUL included, into `buf`.
    let len = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    // 0: error; more than fits: truncated.
    if len == 0 || len > buf.len() {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len - 1])))
}

/// `~/Library/Caches/nibwarden`: the system does not clean it while we run, unlike the
/// temporary directory, whose files are removed after days without access (a listening
/// socket would vanish). A long home path falls back to the temporary directory.
pub fn runtime_dir() -> Option<PathBuf> {
    pick_runtime_dir(
        home().ok().as_deref(),
        user_temp_dir().or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from)),
    )
}

fn pick_runtime_dir(home: Option<&Path>, temp: Option<PathBuf>) -> Option<PathBuf> {
    let fits = |dir: &Path| dir.as_os_str().len() <= MAX_RUNTIME_DIR;
    home.map(|h| h.join("Library/Caches/nibwarden"))
        .filter(|d| fits(d))
        .or_else(|| temp.map(|t| t.join("nibwarden")).filter(|d| fits(d)))
}

/// `~/Library/Application Support/nibwarden`. The XDG variables are not honoured: the
/// daemon runs under launchd without them and must agree with the CLI.
pub fn data_dir() -> io::Result<PathBuf> {
    Ok(home()?.join("Library/Application Support/nibwarden"))
}

/// The same directory as the data: macOS has no separate place for settings that are
/// not property lists.
pub fn config_dir() -> io::Result<PathBuf> {
    data_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_caches_unless_too_long() {
        let temp = Some(PathBuf::from("/var/folders/ab/cdefgh/T"));
        assert_eq!(
            pick_runtime_dir(Some(Path::new("/Users/me")), temp.clone()),
            Some(PathBuf::from("/Users/me/Library/Caches/nibwarden"))
        );
        let long = format!("/Users/{}", "x".repeat(60));
        assert_eq!(
            pick_runtime_dir(Some(Path::new(&long)), temp),
            Some(PathBuf::from("/var/folders/ab/cdefgh/T/nibwarden"))
        );
        assert_eq!(pick_runtime_dir(Some(Path::new(&long)), None), None);
    }

    #[test]
    fn the_real_runtime_dir_fits_a_socket() {
        let dir = runtime_dir().unwrap();
        assert!(dir.join("daemon.sock").as_os_str().len() < 104);
    }

    #[test]
    fn data_dir_is_under_the_home() {
        assert!(
            data_dir()
                .unwrap()
                .ends_with("Library/Application Support/nibwarden")
        );
    }
}
