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

/// Registry values under `HKEY_CURRENT_USER`. Names are `Software\...` key paths.
pub mod registry {
    use std::{ffi::OsStr, io};

    use windows::{
        Win32::{
            Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
            System::Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE,
                REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteKeyW,
                RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
            },
        },
        core::PCWSTR,
    };

    use super::wide;

    fn check(code: WIN32_ERROR) -> io::Result<()> {
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code.0 as i32))
        }
    }

    /// An open key, closed on drop.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the key was opened by us.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn open(
        subkey: &str,
        access: windows::Win32::System::Registry::REG_SAM_FLAGS,
    ) -> io::Result<Key> {
        let subkey = wide(OsStr::new(subkey));
        let mut key = HKEY::default();
        // SAFETY: `subkey` is NUL-terminated; `key` receives the handle.
        check(unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                access,
                &mut key,
            )
        })?;
        Ok(Key(key))
    }

    fn missing(e: &io::Error) -> bool {
        e.kind() == io::ErrorKind::NotFound
            || e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND.0 as i32)
    }

    /// Sets the string value `name` (the key's default value for `None`), creating the key.
    pub fn set_string(subkey: &str, name: Option<&str>, value: &str) -> io::Result<()> {
        let data: Vec<u8> = wide(OsStr::new(value))
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        set_raw(subkey, name, REG_SZ.0, &data)
    }

    /// Sets a value of type `kind` (`REG_SZ` is 1, `REG_BINARY` 3), creating the key.
    pub fn set_raw(subkey: &str, name: Option<&str>, kind: u32, data: &[u8]) -> io::Result<()> {
        let subkey = wide(OsStr::new(subkey));
        let mut key = HKEY::default();
        // SAFETY: `subkey` is NUL-terminated; `key` receives the handle.
        check(unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        })?;
        let key = Key(key);
        let name = name.map(|n| wide(OsStr::new(n)));
        // SAFETY: `name` is NUL-terminated or null.
        check(unsafe {
            RegSetValueExW(
                key.0,
                name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
                None,
                REG_VALUE_TYPE(kind),
                Some(data),
            )
        })
    }

    /// The raw value `name` (the key's default for `None`); `None` if the key or value
    /// does not exist.
    pub fn get_raw(subkey: &str, name: Option<&str>) -> io::Result<Option<(u32, Vec<u8>)>> {
        let key = match open(subkey, KEY_QUERY_VALUE) {
            Err(e) if missing(&e) => return Ok(None),
            other => other?,
        };
        let name = name.map(|n| wide(OsStr::new(n)));
        let name = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
        let mut kind = REG_VALUE_TYPE(0);
        let mut len = 0u32;
        // SAFETY: the first call asks for the size; the second reads that many bytes.
        unsafe {
            match check(RegQueryValueExW(
                key.0,
                name,
                None,
                Some(&mut kind),
                None,
                Some(&mut len),
            )) {
                Err(e) if missing(&e) => return Ok(None),
                other => other?,
            }
            let mut data = vec![0u8; len as usize];
            check(RegQueryValueExW(
                key.0,
                name,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut len),
            ))?;
            data.truncate(len as usize);
            Ok(Some((kind.0, data)))
        }
    }

    /// A string value (`REG_SZ`), or `None` if there is none.
    pub fn get_string(subkey: &str, name: Option<&str>) -> io::Result<Option<String>> {
        Ok(get_raw(subkey, name)?.and_then(|(kind, data)| {
            if kind != REG_SZ.0 {
                return None;
            }
            let units: Vec<u16> = data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect();
            let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            Some(String::from_utf16_lossy(&units[..end]))
        }))
    }

    /// Deletes a value; one that is not there is fine.
    pub fn delete_value(subkey: &str, name: &str) -> io::Result<()> {
        let key = match open(subkey, KEY_SET_VALUE) {
            Err(e) if missing(&e) => return Ok(()),
            other => other?,
        };
        let name = wide(OsStr::new(name));
        // SAFETY: `name` is NUL-terminated.
        match check(unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) }) {
            Err(e) if missing(&e) => Ok(()),
            other => other,
        }
    }

    /// Deletes a key without subkeys; one that is not there is fine.
    pub fn delete_key(subkey: &str) -> io::Result<()> {
        let subkey = wide(OsStr::new(subkey));
        // SAFETY: `subkey` is NUL-terminated.
        match check(unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr())) }) {
            Err(e) if missing(&e) => Ok(()),
            other => other,
        }
    }

    /// Quotes `arg` for a command line the way `CommandLineToArgvW` (and the C runtime)
    /// reads it back.
    pub fn quote_arg(arg: &str) -> String {
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
            return arg.to_owned();
        }
        let mut quoted = String::from("\"");
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    // Backslashes before a quote are doubled, and the quote escaped.
                    quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                c => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes));
                    quoted.push(c);
                    backslashes = 0;
                }
            }
        }
        // Backslashes before the closing quote are doubled.
        quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
        quoted.push('"');
        quoted
    }
}

/// Programs started at login: values of `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`,
/// which Settings > Apps > Startup lists, and which the user can switch off there (Windows
/// records that in `...\Explorer\StartupApproved\Run`, and `is_set` honours it).
pub mod run_key {
    use std::io;

    use super::registry;

    /// The two keys involved.
    pub struct Keys<'a> {
        pub run: &'a str,
        pub approved: &'a str,
    }

    /// Where Windows looks.
    pub const REAL: Keys<'static> = Keys {
        run: r"Software\Microsoft\Windows\CurrentVersion\Run",
        approved: r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
    };

    impl Keys<'_> {
        /// `command`: program and arguments, quoted into the command line.
        pub fn set(&self, name: &str, command: &[String], on: bool) -> io::Result<()> {
            if !on {
                registry::delete_value(self.run, name)?;
                return registry::delete_value(self.approved, name);
            }
            let line = command
                .iter()
                .map(|arg| registry::quote_arg(arg))
                .collect::<Vec<_>>()
                .join(" ");
            registry::set_string(self.run, Some(name), &line)?;
            // Turned on again here, so it is no longer "disabled in Startup apps".
            registry::delete_value(self.approved, name)
        }

        pub fn command_line(&self, name: &str) -> Option<String> {
            registry::get_string(self.run, Some(name)).ok().flatten()
        }

        /// In the Run key, and not switched off in Startup apps (a first byte with the low
        /// bit set, `03`, means disabled; `02` and `06` enabled).
        pub fn is_set(&self, name: &str) -> bool {
            if self.command_line(name).is_none() {
                return false;
            }
            let disabled = registry::get_raw(self.approved, Some(name))
                .ok()
                .flatten()
                .is_some_and(|(_, data)| data.first().is_some_and(|b| b & 1 == 1));
            !disabled
        }
    }

    pub fn set(name: &str, command: &[String], on: bool) -> io::Result<()> {
        REAL.set(name, command, on)
    }

    pub fn is_set(name: &str) -> bool {
        REAL.is_set(name)
    }
}

/// Tests: whether the last entry of the SDDL `sddl` names the current user, however the
/// system spells it (`S-1-5-21-…`, or an alias such as `LA` for the built-in
/// Administrator, which CI runs as).
#[cfg(test)]
pub fn last_trustee_is_current_user(sddl: &str) -> bool {
    use windows::Win32::Security::Authorization::ConvertStringSidToSidW;

    let Some(trustee) = sddl
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit(';').next())
    else {
        return false;
    };
    let text = wide(OsStr::new(trustee));
    let mut sid = PSID::default();
    // SAFETY: `text` is NUL-terminated; the SID returned is LocalAlloc'd, copied and freed.
    unsafe {
        if ConvertStringSidToSidW(PCWSTR(text.as_ptr()), &mut sid).is_err() {
            return false;
        }
        let ours = Sid::copy_of(sid).is_ok_and(|sid| Some(sid) == current_user().ok());
        let _ = LocalFree(Some(HLOCAL(sid.0)));
        ours
    }
}

/// Tests: whether this process has `SeDebugPrivilege` enabled, as an elevated
/// administrator's may: it opens any process whatever its DACL says.
#[cfg(test)]
pub fn debug_privilege_enabled() -> bool {
    use windows::{
        Win32::{
            Foundation::{CloseHandle, LUID},
            Security::{
                LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, TOKEN_PRIVILEGES, TokenPrivileges,
            },
        },
        core::w,
    };

    // SAFETY: the token handle is ours and closed; the buffer is sized by the first call
    // and read as the TOKEN_PRIVILEGES it holds.
    unsafe {
        let mut debug = LUID::default();
        if LookupPrivilegeValueW(PCWSTR::null(), w!("SeDebugPrivilege"), &mut debug).is_err() {
            return false;
        }
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut size = 0;
        let _ = GetTokenInformation(token, TokenPrivileges, None, 0, &mut size);
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let ok = GetTokenInformation(
            token,
            TokenPrivileges,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(token);
        if !ok {
            return false;
        }
        let privileges = &*buffer.as_ptr().cast::<TOKEN_PRIVILEGES>();
        let all = std::slice::from_raw_parts(
            privileges.Privileges.as_ptr(),
            privileges.PrivilegeCount as usize,
        );
        all.iter().any(|p| {
            p.Luid.LowPart == debug.LowPart
                && p.Luid.HighPart == debug.HighPart
                && p.Attributes.contains(SE_PRIVILEGE_ENABLED)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{registry, run_key};

    fn scratch(name: &str) -> String {
        format!(r"Software\NibwardenTest-{}-{name}", std::process::id())
    }

    #[test]
    fn quotes_like_the_c_runtime_reads() {
        let q = registry::quote_arg;
        assert_eq!(q("plain"), "plain");
        assert_eq!(
            q(r"C:\Program Files\n w\a.exe"),
            r#""C:\Program Files\n w\a.exe""#
        );
        assert_eq!(q(""), r#""""#);
        assert_eq!(q(r#"a"b"#), r#""a\"b""#);
        // Backslashes before the closing quote are doubled; others are left alone.
        assert_eq!(q(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(q(r#"a\"b c"#), r#""a\\\"b c""#);
    }

    #[test]
    fn registry_values_round_trip() {
        let key = scratch("values");
        assert_eq!(registry::get_string(&key, Some("v")).unwrap(), None);
        registry::set_string(&key, Some("v"), "héllo wörld").unwrap();
        registry::set_string(&key, None, "default").unwrap();
        assert_eq!(
            registry::get_string(&key, Some("v")).unwrap().as_deref(),
            Some("héllo wörld")
        );
        assert_eq!(
            registry::get_string(&key, None).unwrap().as_deref(),
            Some("default")
        );
        registry::delete_value(&key, "v").unwrap();
        registry::delete_value(&key, "v").unwrap();
        assert_eq!(registry::get_string(&key, Some("v")).unwrap(), None);
        registry::delete_key(&key).unwrap();
        registry::delete_key(&key).unwrap();
        assert_eq!(registry::get_string(&key, None).unwrap(), None);
    }

    #[test]
    fn run_entries_follow_startup_apps() {
        let (run, approved) = (scratch("run"), scratch("approved"));
        let keys = run_key::Keys {
            run: &run,
            approved: &approved,
        };
        assert!(!keys.is_set("x"));
        keys.set("x", &[r"C:\a b\x.exe".into(), "tray".into()], true)
            .unwrap();
        assert!(keys.is_set("x"));
        assert_eq!(
            keys.command_line("x").as_deref(),
            Some(r#""C:\a b\x.exe" tray"#)
        );
        // The user switches it off in Settings: Windows writes 03 00 00 00 ...
        registry::set_raw(
            &approved,
            Some("x"),
            3,
            &[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
        assert!(!keys.is_set("x"));
        // Turning it on here enables it again.
        keys.set("x", &["y.exe".into()], true).unwrap();
        assert!(keys.is_set("x"));
        keys.set("x", &[], false).unwrap();
        keys.set("x", &[], false).unwrap();
        assert!(!keys.is_set("x"));
        let _ = registry::delete_key(&run);
        let _ = registry::delete_key(&approved);
    }
}
