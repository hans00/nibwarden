use std::{ffi::OsStr, io};

use windows::{
    Win32::{
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    core::{PCWSTR, w},
};

use crate::winutil::wide;

/// `ShellExecuteW` with the "open" verb: the default browser. `url` is a web address (the
/// interface checks that), which the shell hands to the browser as it is; there is no
/// command line to quote.
pub fn open(url: &str) -> io::Result<()> {
    let url = wide(OsStr::new(url));
    // COM is initialised for the call (the shell's handlers may need it) on a thread of its
    // own, so the caller's thread is not left with an apartment it did not ask for.
    std::thread::spawn(move || {
        // SAFETY: strings are NUL-terminated and outlive the call; COM is paired.
        unsafe {
            let initialised =
                CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok();
            let result = ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(url.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
            if initialised {
                CoUninitialize();
            }
            // Above 32 is success; the rest are error codes.
            let code = result.0 as isize;
            if code > 32 {
                Ok(())
            } else {
                Err(io::Error::other(format!("ShellExecute failed ({code})")))
            }
        }
    })
    .join()
    .map_err(|_| io::Error::other("the browser launcher panicked"))?
}
