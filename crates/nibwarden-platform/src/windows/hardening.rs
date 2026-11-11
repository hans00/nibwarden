//! What Linux gets from `PR_SET_DUMPABLE`, `RLIMIT_CORE` and Yama, as far as Windows has it:
//!
//! - Crash dumps: no error dialog (`SetErrorMode`), Windows Error Reporting told to leave
//!   the heap out (`WerSetFlags`), and an unhandled-exception filter that ends the process
//!   without WER seeing it. Not covered: an administrator's `LocalDumps` registry
//!   configuration (`DumpType`) is applied by the WER service regardless, and `abort()`
//!   (`__fastfail`) skips the filter. Key material is not in marked regions either: WER's
//!   `WerRegisterExcludedMemoryBlock` works per region, and Rust's heap is not one.
//! - Other processes of the same user reading or writing our memory (what ptrace is on
//!   Linux): the process's own DACL grants the user only querying, waiting for and
//!   terminating it, and `OWNER RIGHTS` is cut down to reading the descriptor, so not even
//!   the owner's implicit `WRITE_DAC` remains to widen it again. Not covered: a process with
//!   `SeDebugPrivilege` (an elevated administrator) or SYSTEM, which bypass DACLs.
//! - Code injection by configuration: extension points (AppInit DLLs, legacy IME hooks)
//!   are off, images load neither from network shares nor from low-integrity locations, and
//!   prefer System32.

use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use windows::{
    Win32::{
        Foundation::{HANDLE, HLOCAL, LocalFree},
        Security::{
            ACL,
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                SE_KERNEL_OBJECT, SetSecurityInfo,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
        Storage::FileSystem::{READ_CONTROL, WRITE_DAC},
        System::{
            Diagnostics::Debug::{
                EXCEPTION_POINTERS, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
                SEM_NOOPENFILEERRORBOX, SetErrorMode, SetUnhandledExceptionFilter,
            },
            ErrorReporting::{WER_FAULT_REPORTING_FLAG_NOHEAP, WerSetFlags},
            Threading::{
                GetCurrentProcess, GetCurrentProcessId, OpenProcess,
                ProcessExtensionPointDisablePolicy, ProcessImageLoadPolicy,
                SetProcessMitigationPolicy, TerminateProcess,
            },
        },
    },
    core::PCWSTR,
};

use crate::winutil::{IoResult as _, current_user, wide};

/// `PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE`.
const ALLOWED_ON_PROCESS: u32 = 0x0000_0001 | 0x0000_1000 | 0x0010_0000;

pub fn harden_process() {
    // SAFETY: plain calls; the filter is a function without captured state.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
        if WerSetFlags(WER_FAULT_REPORTING_FLAG_NOHEAP).is_err() {
            eprintln!("warning: could not keep the heap out of crash reports");
        }
        SetUnhandledExceptionFilter(Some(die_quietly));
    }
    if let Err(e) = protect_process() {
        eprintln!("warning: could not restrict access to the process: {e}");
    }
    mitigations();
}

/// Ends the process without Windows Error Reporting taking a dump of it.
unsafe extern "system" fn die_quietly(_: *const EXCEPTION_POINTERS) -> i32 {
    // SAFETY: ends this process; STATUS_FATAL_APP_EXIT.
    unsafe {
        let _ = TerminateProcess(GetCurrentProcess(), 0x4000_0015);
    }
    1 // EXCEPTION_EXECUTE_HANDLER
}

fn protect_process() -> std::io::Result<()> {
    let user = current_user()?.to_sddl()?;
    let sddl = wide(std::ffi::OsStr::new(&format!(
        "D:P(A;;{ALLOWED_ON_PROCESS:#x};;;{user})(A;;RC;;;OW)"
    )));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let mut present = false.into();
    let mut defaulted = false.into();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated; the descriptor is freed below, after the DACL
    // inside it was applied; the real handle to ourselves is owned by `process`.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .io()?;
        let result = (|| -> windows::core::Result<()> {
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)?;
            // The pseudo handle from GetCurrentProcess is not meant for this.
            let process = OwnedHandle::from_raw_handle(
                OpenProcess(
                    windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS(
                        READ_CONTROL.0 | WRITE_DAC.0,
                    ),
                    false,
                    GetCurrentProcessId(),
                )?
                .0,
            );
            SetSecurityInfo(
                HANDLE(process.as_raw_handle()),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
            .ok()
        })();
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        result.io()
    }
}

fn mitigations() {
    // Bit 0 of the policy's flags: DisableExtensionPoints.
    let extension_points = 1u32;
    // NoRemoteImages | NoLowMandatoryLabelImages | PreferSystem32Images.
    let image_load = 0b111u32;
    for (policy, flags) in [
        (ProcessExtensionPointDisablePolicy, extension_points),
        (ProcessImageLoadPolicy, image_load),
    ] {
        // SAFETY: the buffer is the one DWORD of flags both policies are made of.
        let result = unsafe {
            SetProcessMitigationPolicy(policy, (&raw const flags).cast(), size_of::<u32>())
        };
        if let Err(e) = result {
            eprintln!("warning: could not enable a process mitigation: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    /// Affects the whole test process, which keeps working: querying stays allowed.
    #[test]
    fn others_of_the_user_cannot_read_the_process() {
        super::harden_process();
        // SAFETY: opens and closes handles to this process.
        unsafe {
            let pid = GetCurrentProcessId();
            let limited = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
            assert!(limited.is_ok());
            let _ = windows::Win32::Foundation::CloseHandle(limited.unwrap());
            if !crate::winutil::under_wine() {
                assert!(OpenProcess(PROCESS_VM_READ, false, pid).is_err());
            }
        }
    }
}
