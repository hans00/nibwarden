//! Helpers shared by the Windows implementations: the current user's SID, and a security
//! descriptor that lets only that user in.

use std::{
    ffi::{OsStr, c_void},
    io,
    os::windows::{
        ffi::OsStrExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    },
};

use std::path::PathBuf;

use windows::{
    Win32::{
        Foundation::{HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetLengthSid, GetTokenInformation, IsValidSid, PSECURITY_DESCRIPTOR, PSID,
            SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::Com::CoTaskMemFree,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
        UI::Shell::{
            FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_Windows, KF_FLAG_DEFAULT,
            SHGetKnownFolderPath,
        },
    },
    core::{GUID, PCWSTR, PWSTR},
};

/// NUL-terminated UTF-16.
pub fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// A user's SID in its binary form.
#[derive(Clone, PartialEq, Eq)]
pub struct Sid(Vec<u8>);

impl Sid {
    pub fn psid(&self) -> PSID {
        PSID(self.0.as_ptr().cast_mut().cast())
    }

    /// The `S-1-5-21-...` form.
    pub fn to_sddl(&self) -> io::Result<String> {
        let mut out = PWSTR::null();
        // SAFETY: the SID is a valid copy of one; `out` receives a LocalAlloc'd string,
        // which is copied and freed here.
        unsafe {
            ConvertSidToStringSidW(self.psid(), &mut out).io()?;
            let text = String::from_utf16_lossy(out.as_wide());
            let _ = LocalFree(Some(HLOCAL(out.0.cast())));
            Ok(text)
        }
    }

    /// Copies the SID `sid` points to.
    ///
    /// # Safety
    /// `sid` points to a valid SID.
    pub unsafe fn copy_of(sid: PSID) -> io::Result<Self> {
        // SAFETY: as the caller promises.
        unsafe {
            if !IsValidSid(sid).as_bool() {
                return Err(io::Error::other("invalid SID"));
            }
            let len = GetLengthSid(sid) as usize;
            Ok(Self(
                std::slice::from_raw_parts(sid.0.cast::<u8>(), len).to_vec(),
            ))
        }
    }
}

impl std::fmt::Debug for Sid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_sddl().unwrap_or_else(|_| "?".into()))
    }
}

/// The user of the access token of `process` (a handle with
/// `PROCESS_QUERY_LIMITED_INFORMATION`).
pub fn process_user(process: HANDLE) -> io::Result<Sid> {
    let mut raw = HANDLE::default();
    // SAFETY: `raw` receives a new handle, owned from here on.
    let token = unsafe {
        OpenProcessToken(process, TOKEN_QUERY, &mut raw).io()?;
        OwnedHandle::from_raw_handle(raw.0)
    };
    let handle = HANDLE(token.as_raw_handle());
    let mut len = 0u32;
    // SAFETY: the first call only reports the size needed (and fails doing so).
    let _ = unsafe { GetTokenInformation(handle, TokenUser, None, 0, &mut len) };
    // u64 elements: TOKEN_USER wants pointer alignment.
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` has at least `len` bytes; on success it holds a TOKEN_USER and the
    // SID it points to.
    unsafe {
        GetTokenInformation(
            handle,
            TokenUser,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            len,
            &mut len,
        )
        .io()?;
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        Sid::copy_of(user.User.Sid)
    }
}

/// The user this process runs as.
pub fn current_user() -> io::Result<Sid> {
    // SAFETY: the pseudo handle needs no closing.
    process_user(unsafe { GetCurrentProcess() })
}

/// A security descriptor, and the attributes that refer to it, granting `access` (an SDDL
/// rights string such as `GA`) to the current user alone. The DACL is protected, so
/// nothing is inherited from the parent either.
pub struct OwnerOnly {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl OwnerOnly {
    /// `inherit`: children of a directory get the same access (objects and containers).
    pub fn new(access: &str, inherit: bool) -> io::Result<Self> {
        let sid = current_user()?.to_sddl()?;
        let flags = if inherit { "OICI" } else { "" };
        let sddl = wide(OsStr::new(&format!("D:P(A;{flags};{access};;;{sid})")));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is NUL-terminated; the descriptor is LocalAlloc'd and freed in Drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
            .io()?;
        }
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            },
        })
    }

    /// For `CreateFileW` and the like; valid while `self` lives.
    pub fn attributes(&self) -> *const SECURITY_ATTRIBUTES {
        &raw const self.attributes
    }

    /// For tokio's named pipe options; valid while `self` lives.
    pub fn attributes_mut(&mut self) -> *mut c_void {
        (&raw mut self.attributes).cast()
    }

    pub fn descriptor(&self) -> PSECURITY_DESCRIPTOR {
        self.descriptor
    }
}

impl Drop for OwnerOnly {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.descriptor.0)));
        }
    }
}

/// A known folder of the current user or the system, from the shell (not from the
/// environment, which any parent process may have set to anything).
pub fn known_folder(id: &GUID) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    // SAFETY: on success the shell returns a NUL-terminated string that we copy and free.
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).io()?;
        let dir = PathBuf::from(std::ffi::OsString::from_wide(path.as_wide()));
        CoTaskMemFree(Some(path.0.cast()));
        Ok(dir)
    }
}

/// `%ProgramFiles%`, `%ProgramFiles(x86)%` and `%SystemRoot%`.
pub fn system_program_dirs() -> Vec<PathBuf> {
    [
        &FOLDERID_ProgramFiles,
        &FOLDERID_ProgramFilesX86,
        &FOLDERID_Windows,
    ]
    .into_iter()
    .filter_map(|id| known_folder(id).ok())
    .collect()
}

/// Whether this is Wine rather than Windows, whose security descriptors on files and named
/// pipe `FILE_FLAG_FIRST_PIPE_INSTANCE` are not (fully) implemented: tests that check those
/// skip the assertions Wine cannot satisfy.
#[cfg(test)]
pub fn under_wine() -> bool {
    use windows::{
        Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
        core::{PCSTR, w},
    };
    // SAFETY: looks a name up in a module that every process has loaded.
    unsafe {
        GetModuleHandleW(w!("ntdll.dll"))
            .ok()
            .and_then(|ntdll| GetProcAddress(ntdll, PCSTR(c"wine_get_version".as_ptr().cast())))
            .is_some()
    }
}

/// A Windows API error as the `io::Error` it means. The `windows` crate's own conversion
/// keeps the HRESULT as the OS error code, so `kind()` of "file not found" would be
/// `Uncategorized`.
pub fn io_error(e: windows::core::Error) -> io::Error {
    let code = e.code().0 as u32;
    // HRESULT_FROM_WIN32: facility 7 (win32), severity bit set.
    if code >> 16 == 0x8007 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other(e)
    }
}

pub trait IoResult<T> {
    fn io(self) -> io::Result<T>;
}

impl<T> IoResult<T> for windows::core::Result<T> {
    fn io(self) -> io::Result<T> {
        self.map_err(io_error)
    }
}
