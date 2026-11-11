use std::{
    fs::File,
    io,
    os::windows::{
        ffi::OsStrExt as _,
        io::{AsRawHandle as _, FromRawHandle as _},
    },
    path::{Component, Path, PathBuf},
};

use windows::{
    Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, HANDLE, HLOCAL, LocalFree},
        Security::{
            ACL,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo},
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, OWNER_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        },
        Storage::FileSystem::{
            CREATE_ALWAYS, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_ALWAYS, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
        },
    },
    core::PCWSTR,
};

use crate::winutil::{IoResult as _, OwnerOnly, Sid, current_user, wide};

/// Full control for the owner alone.
const FULL: &str = "FA";

fn is_trusted_owner(owner: &Sid, user: &Sid) -> bool {
    // Elevated processes own what they create as Administrators.
    owner == user
        || owner
            .to_sddl()
            .is_ok_and(|s| matches!(s.as_str(), "S-1-5-32-544" | "S-1-5-18"))
}

/// The owner of the object `handle` (opened with READ_CONTROL).
fn owner_of(handle: HANDLE) -> io::Result<Sid> {
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: outputs are valid; the owner points into `descriptor`, which is freed after
    // the SID is copied.
    unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut descriptor),
        )
        .ok()
        .io()?;
        let sid = Sid::copy_of(owner);
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        sid
    }
}

/// Replaces the DACL of the object `handle` (opened with WRITE_DAC) by `security`'s, and
/// stops it inheriting from the parent.
fn apply(handle: HANDLE, security: &OwnerOnly) -> io::Result<()> {
    let mut present = false.into();
    let mut defaulted = false.into();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: the descriptor lives as long as `security`; the ACL is read in place.
    unsafe {
        GetSecurityDescriptorDacl(
            security.descriptor(),
            &mut present,
            &mut dacl,
            &mut defaulted,
        )
        .io()?;
        SetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(dacl),
            None,
        )
        .ok()
        .io()?;
    }
    Ok(())
}

/// Opens `path` (a file or directory) for changing and reading its security.
fn open_for_security(path: &Path) -> io::Result<File> {
    let path = wide(path.as_os_str());
    // SAFETY: `path` is NUL-terminated; the handle is owned by the returned File.
    unsafe {
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            (READ_CONTROL | WRITE_DAC).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
        .io()?;
        Ok(File::from_raw_handle(handle.0))
    }
}

pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let security = OwnerOnly::new(FULL, true)?;
    let name = wide(dir.as_os_str());
    // Created with the descriptor, so there is no moment when others could enter.
    // SAFETY: `name` is NUL-terminated; the attributes live as long as `security`.
    let created = unsafe { CreateDirectoryW(PCWSTR(name.as_ptr()), Some(security.attributes())) };
    match created {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => {}
        Err(e) => return Err(e.into()),
    }
    // A new directory has the descriptor already; one that existed must be ours, and is
    // made private. Both are checked and set (the same way) in the end.
    let handle = open_for_security(dir)?;
    let handle_raw = HANDLE(handle.as_raw_handle());
    let owner = owner_of(handle_raw)?;
    if !is_trusted_owner(&owner, &current_user()?) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by another user", dir.display()),
        ));
    }
    apply(handle_raw, &security)
}

pub fn restrict_file(path: &Path) -> io::Result<()> {
    let file = match open_for_security(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    apply(HANDLE(file.as_raw_handle()), &OwnerOnly::new(FULL, false)?)
}

/// Creates or opens `path` with `disposition`, with the owner-only descriptor if it is
/// created, and makes sure the DACL is the owner-only one in any case.
fn open_private(
    path: &Path,
    disposition: windows::Win32::Storage::FileSystem::FILE_CREATION_DISPOSITION,
) -> io::Result<File> {
    let security = OwnerOnly::new(FULL, false)?;
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL-terminated; the attributes live as long as `security`; the
    // handle is owned by the returned File.
    let file = unsafe {
        let handle = CreateFileW(
            PCWSTR(name.as_ptr()),
            FILE_GENERIC_WRITE.0 | WRITE_DAC.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            Some(security.attributes()),
            disposition,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .io()?;
        File::from_raw_handle(handle.0)
    };
    // A file that existed keeps the DACL it had.
    apply(HANDLE(file.as_raw_handle()), &security)?;
    Ok(file)
}

pub fn create_private_file(path: &Path) -> io::Result<File> {
    open_private(path, CREATE_ALWAYS)
}

pub fn try_lock(path: &Path) -> io::Result<Option<File>> {
    // `File::try_lock` is LockFileEx with LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY.
    let file = open_private(path, OPEN_ALWAYS)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Under `%ProgramFiles%`, `%ProgramFiles(x86)%` or `%SystemRoot%`, which ordinary users
/// cannot write to.
pub fn is_system_path(path: &Path) -> bool {
    let Some(path) = normalized(path) else {
        return false;
    };
    crate::winutil::system_program_dirs()
        .iter()
        .filter_map(|dir| normalized(dir))
        .any(|dir| path.starts_with(dir))
}

/// Absolute, without `..`, `\\?\` prefix and case: Windows paths compare ignoring case.
fn normalized(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    let text: Vec<u16> = path.as_os_str().encode_wide().collect();
    let text = String::from_utf16_lossy(&text);
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
        .unwrap_or(text);
    Some(PathBuf::from(text.to_lowercase()))
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nw-fs-{}-{name}", std::process::id()))
    }

    #[test]
    fn system_paths() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        assert!(is_system_path(&Path::new(&root).join(r"System32\cmd.exe")));
        // Case does not matter, and `..` does not get out of the way.
        assert!(is_system_path(
            &Path::new(&root.to_uppercase()).join(r"system32\CMD.EXE")
        ));
        assert!(!is_system_path(
            &Path::new(&root).join(r"System32\..\..\Users")
        ));
        assert!(!is_system_path(&std::env::current_exe().unwrap()));
        assert!(!is_system_path(&std::env::temp_dir()));
        assert!(!is_system_path(Path::new("relative")));
    }

    #[test]
    fn lock_is_exclusive_until_dropped() {
        let path = temp("lock");
        let held = try_lock(&path).unwrap();
        assert!(held.is_some());
        assert!(try_lock(&path).unwrap().is_none());
        drop(held);
        assert!(try_lock(&path).unwrap().is_some());
        std::fs::remove_file(path).unwrap();
    }

    /// The SDDL of a file's DACL.
    fn dacl(path: &Path) -> String {
        use windows::Win32::Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
                SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            DACL_SECURITY_INFORMATION,
        };
        let name = wide(path.as_os_str());
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let mut text = windows::core::PWSTR::null();
        // SAFETY: outputs are valid; both allocations are freed.
        unsafe {
            GetNamedSecurityInfoW(
                PCWSTR(name.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                &mut descriptor,
            )
            .ok()
            .unwrap();
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                None,
            )
            .unwrap();
            let sddl = String::from_utf16_lossy(text.as_wide());
            let _ = LocalFree(Some(HLOCAL(text.0.cast())));
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            sddl
        }
    }

    fn only_us(sddl: &str) {
        if crate::winutil::under_wine() {
            // Wine does not keep the DACL of a file or directory (not on every filesystem).
            return;
        }
        let sid = current_user().unwrap().to_sddl().unwrap();
        assert!(sddl.starts_with("D:P"), "{sddl}");
        assert!(sddl.ends_with(&format!(";;;{sid})")), "{sddl}");
        assert_eq!(sddl.matches("(A;").count(), 1, "{sddl}");
        assert_eq!(sddl.matches("(D;").count(), 0, "{sddl}");
    }

    #[test]
    fn private_files_and_dirs_admit_only_us() {
        let dir = temp("private").join("a").join("b");
        create_private_dir(&dir).unwrap();
        only_us(&dacl(&dir));
        // Again, now that it exists.
        create_private_dir(&dir).unwrap();

        let file = dir.join("f.txt");
        {
            use std::io::Write as _;
            let mut f = create_private_file(&file).unwrap();
            f.write_all(b"secret").unwrap();
        }
        only_us(&dacl(&file));
        assert_eq!(std::fs::read(&file).unwrap(), b"secret");
        // A file made the ordinary way inherits more; restricting mends that.
        let other = dir.join("g.txt");
        std::fs::write(&other, "x").unwrap();
        restrict_file(&other).unwrap();
        only_us(&dacl(&other));
        // Creating again truncates.
        drop(create_private_file(&file).unwrap());
        assert_eq!(std::fs::read(&file).unwrap(), b"");
        // A missing file is fine.
        restrict_file(&dir.join("missing")).unwrap();
        std::fs::remove_dir_all(temp("private")).unwrap();
    }
}
