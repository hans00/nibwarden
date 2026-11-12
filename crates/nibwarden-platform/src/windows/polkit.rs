//! System authentication with Windows Hello (`UserConsentVerifier`): a PIN, a fingerprint or
//! a face, whatever the user set up in Windows.
//!
//! The daemon has no window, and `RequestVerificationAsync` only works for packaged
//! (UWP) apps; a desktop process has to use `IUserConsentVerifierInterop` and name a
//! window to parent the prompt to. That window is the GUI's that asked: `pid`.

use std::io;

use windows::{
    // (IAsyncOperation lives in windows-future)
    Security::Credentials::UI::{
        UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
    },
    Win32::{
        Foundation::{HWND, LPARAM},
        System::WinRT::IUserConsentVerifierInterop,
        UI::WindowsAndMessaging::{
            EnumWindows, GW_OWNER, GWL_EXSTYLE, GetDesktopWindow, GetForegroundWindow, GetWindow,
            GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible, WS_EX_TOOLWINDOW,
        },
    },
    core::{BOOL, HSTRING, factory},
};

/// What the prompt says.
const MESSAGE: &str = "Unlock nibwarden";

pub async fn available() -> bool {
    tokio::task::spawn_blocking(|| {
        UserConsentVerifier::CheckAvailabilityAsync()
            .and_then(|operation| operation.join())
            .is_ok_and(|availability| availability == UserConsentVerifierAvailability::Available)
    })
    .await
    .unwrap_or(false)
}

pub async fn authenticate(pid: u32) -> io::Result<bool> {
    tokio::task::spawn_blocking(move || verify(window_of(pid)))
        .await
        .map_err(io::Error::other)?
}

fn verify(window: HWND) -> io::Result<bool> {
    let interop =
        factory::<UserConsentVerifier, IUserConsentVerifierInterop>().map_err(io::Error::other)?;
    // SAFETY: `window` is a window handle; the call only uses it to place the prompt.
    let operation: windows_future::IAsyncOperation<UserConsentVerificationResult> =
        unsafe { interop.RequestVerificationForWindowAsync(window, &HSTRING::from(MESSAGE)) }
            .map_err(io::Error::other)?;
    outcome(operation.join().map_err(io::Error::other)?)
}

/// Verified is `true`; the user backing out, or running out of attempts, is `false`;
/// anything that says Hello cannot be used here is an error.
fn outcome(result: UserConsentVerificationResult) -> io::Result<bool> {
    match result {
        UserConsentVerificationResult::Verified => Ok(true),
        UserConsentVerificationResult::Canceled
        | UserConsentVerificationResult::RetriesExhausted
        | UserConsentVerificationResult::DeviceBusy => Ok(false),
        other => Err(io::Error::other(format!(
            "Windows Hello cannot confirm the user ({})",
            other.0
        ))),
    }
}

/// A visible top-level window of process `pid` to put the prompt over: the foreground one
/// if it is that process's, else the first; without any, the foreground window of whoever
/// has it, and at last the desktop.
fn window_of(pid: u32) -> HWND {
    struct Search {
        pid: u32,
        found: Option<HWND>,
    }

    unsafe extern "system" fn visit(window: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Search` on the stack of `window_of`, alive for the call.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        // SAFETY: plain queries about a window handle.
        unsafe {
            let mut owner_pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut owner_pid));
            let top_level = GetWindow(window, GW_OWNER).map_or(true, |owner| owner.0.is_null());
            let tool = GetWindowLongW(window, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0;
            if owner_pid == search.pid && IsWindowVisible(window).as_bool() && top_level && !tool {
                search.found = Some(window);
                return BOOL(0);
            }
        }
        BOOL(1)
    }

    // SAFETY: the callback is called only during EnumWindows, with our `Search`.
    unsafe {
        let foreground = GetForegroundWindow();
        let mut owner_pid = 0u32;
        GetWindowThreadProcessId(foreground, Some(&mut owner_pid));
        if owner_pid == pid {
            return foreground;
        }
        let mut search = Search { pid, found: None };
        let _ = EnumWindows(Some(visit), LPARAM((&raw mut search) as isize));
        search.found.unwrap_or(if foreground.0.is_null() {
            GetDesktopWindow()
        } else {
            foreground
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_results() {
        assert!(outcome(UserConsentVerificationResult::Verified).unwrap());
        assert!(!outcome(UserConsentVerificationResult::Canceled).unwrap());
        assert!(!outcome(UserConsentVerificationResult::RetriesExhausted).unwrap());
        assert!(outcome(UserConsentVerificationResult::NotConfiguredForUser).is_err());
        assert!(outcome(UserConsentVerificationResult::DeviceNotPresent).is_err());
        assert!(outcome(UserConsentVerificationResult::DisabledByPolicy).is_err());
    }

    #[test]
    fn a_window_is_found_even_without_one_of_the_process() {
        // The process has no window: falls back to the foreground one or the desktop.
        let window = window_of(std::process::id());
        let _ = window;
    }

    #[tokio::test]
    async fn availability_is_a_plain_answer() {
        // True on a machine with Hello set up, false on CI runners and Wine.
        let _ = available().await;
    }
}
