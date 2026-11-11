//! A hidden message-only window on a thread of its own, which receives
//!
//! - `WM_WTSSESSION_CHANGE` with `WTS_SESSION_LOCK` for this session (registered with
//!   `WTSRegisterSessionNotification`), when the screen locks, and
//! - `WM_POWERBROADCAST` with `PBT_APMSUSPEND` (registered with
//!   `RegisterSuspendResumeNotification`, since message-only windows get no broadcasts),
//!   when the machine is about to suspend or hibernate.
//!
//! Systems with Modern Standby (S0 low-power idle) send no `PBT_APMSUSPEND`; there the
//! screen locking, which is what usually precedes it, is what locks the vault.

use std::{io, ptr};

use tokio::sync::{mpsc::UnboundedSender, oneshot};
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            Power::{RegisterSuspendResumeNotification, UnregisterSuspendResumeNotification},
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
                WTSUnRegisterSessionNotification,
            },
        },
        UI::WindowsAndMessaging::{
            CREATESTRUCTW, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW,
            DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, HWND_MESSAGE, MSG,
            PostMessageW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW, TranslateMessage,
            WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY,
            WM_POWERBROADCAST, WNDCLASSW,
        },
    },
    core::w,
};

use super::SessionEvent;

/// `WM_WTSSESSION_CHANGE`
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
/// `WTS_SESSION_LOCK`
const WTS_SESSION_LOCK: usize = 7;
/// `PBT_APMSUSPEND`
const PBT_APMSUSPEND: usize = 0x0004;

pub(super) const CLASS_NAME: windows::core::PCWSTR = w!("NibwardenSessionEvents");

/// What a window message means for us.
pub(super) fn event_for(message: u32, wparam: usize) -> Option<SessionEvent> {
    match (message, wparam) {
        (WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK) => Some(SessionEvent::ScreenLocked),
        (WM_POWERBROADCAST, PBT_APMSUSPEND) => Some(SessionEvent::Sleeping),
        _ => None,
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the user data is the sender stored at WM_NCCREATE, which the window's thread
    // keeps alive until the window is gone, and cleared at WM_NCDESTROY.
    unsafe {
        match message {
            WM_NCCREATE => {
                let create = &*(lparam.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
                DefWindowProcW(window, message, wparam, lparam)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                DefWindowProcW(window, message, wparam, lparam)
            }
            _ => {
                if let Some(event) = event_for(message, wparam.0) {
                    let sender = GetWindowLongPtrW(window, GWLP_USERDATA)
                        as *const UnboundedSender<SessionEvent>;
                    if let Some(sender) = sender.as_ref() {
                        let _ = sender.send(event);
                    }
                    // TRUE: for WM_POWERBROADCAST, "grant the request".
                    return LRESULT(1);
                }
                DefWindowProcW(window, message, wparam, lparam)
            }
        }
    }
}

/// Creates the window. `sender` is stored in the window for `window_proc` and must outlive
/// it: the thread that runs the window keeps it.
fn create(sender: *const UnboundedSender<SessionEvent>) -> io::Result<HWND> {
    // SAFETY: a window class and window of this module; `sender` is valid as long as the
    // window exists, as promised.
    unsafe {
        let instance = GetModuleHandleW(None).map_err(io::Error::other)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // 0 also when the class exists from an earlier watcher in this process, which is
        // fine: creating the window below tells whether it is usable.
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("nibwarden session events"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            Some(sender.cast()),
        )
        .map_err(io::Error::other)
    }
}

/// Runs the window's thread: registers, tells `ready` the window (or why not), pumps
/// messages until the window is closed.
fn run(events: UnboundedSender<SessionEvent>, ready: oneshot::Sender<io::Result<isize>>) {
    // The window refers to this; it is dropped after the window is gone.
    let events = Box::new(events);
    let window = match create(&raw const *events) {
        Ok(window) => window,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // SAFETY: registrations for our window, undone below before it is destroyed.
    let (session, suspend) = unsafe {
        (
            WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION).is_ok(),
            RegisterSuspendResumeNotification(HANDLE(window.0), DEVICE_NOTIFY_WINDOW_HANDLE).ok(),
        )
    };
    if !session && suspend.is_none() {
        // SAFETY: ends the window we made.
        unsafe {
            let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
            pump();
        }
        let _ = ready.send(Err(io::Error::other(
            "cannot register for session or power notifications",
        )));
        return;
    }
    if !session {
        tracing::info!("no notification when the screen locks; only for sleep");
    }
    if suspend.is_none() {
        tracing::info!("no notification for sleep; only for the screen locking");
    }
    let _ = ready.send(Ok(window.0 as isize));
    // SAFETY: undoes the registrations above, on the thread that made them.
    unsafe {
        pump();
        if let Some(handle) = suspend {
            let _ = UnregisterSuspendResumeNotification(handle);
        }
        if session {
            let _ = WTSUnRegisterSessionNotification(window);
        }
    }
}

/// The message loop, until `WM_QUIT`.
///
/// # Safety
/// Called on the thread that owns the window.
unsafe fn pump() {
    let mut message = MSG::default();
    // SAFETY: as the caller promises.
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

pub async fn watch(events: UnboundedSender<SessionEvent>) -> io::Result<()> {
    let (ready_tx, ready_rx) = oneshot::channel();
    let (done_tx, done_rx) = oneshot::channel::<()>();
    let sender = events.clone();
    std::thread::Builder::new()
        .name("session-events".to_owned())
        .spawn(move || {
            run(sender, ready_tx);
            let _ = done_tx.send(());
        })?;
    let window = ready_rx
        .await
        .map_err(|_| io::Error::other("the session event thread ended"))??;
    tokio::select! {
        // Nobody listens any more: close the window, which ends the thread.
        () = events.closed() => {
            // SAFETY: posting to a window handle is safe even if it is gone by now.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(ptr::with_exposed_provenance_mut(window as usize))),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
        _ = done_rx => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, SendMessageW};

    use super::*;

    #[test]
    fn reads_messages() {
        assert_eq!(
            event_for(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK),
            Some(SessionEvent::ScreenLocked)
        );
        // WTS_SESSION_UNLOCK
        assert_eq!(event_for(WM_WTSSESSION_CHANGE, 8), None);
        assert_eq!(
            event_for(WM_POWERBROADCAST, PBT_APMSUSPEND),
            Some(SessionEvent::Sleeping)
        );
        // PBT_APMRESUMEAUTOMATIC
        assert_eq!(event_for(WM_POWERBROADCAST, 0x0012), None);
    }

    /// Sends the window the messages Windows would, and closes it by dropping the receiver.
    #[tokio::test]
    async fn forwards_messages_and_stops() {
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let watcher = tokio::spawn(watch(tx));
        let find = || {
            // SAFETY: looks for a message-only window of our class.
            unsafe {
                FindWindowExW(Some(HWND_MESSAGE), None, CLASS_NAME, None)
                    .ok()
                    .filter(|w| !w.0.is_null())
            }
        };
        let mut window = None;
        for _ in 0..100 {
            window = find();
            if window.is_some() || watcher.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let Some(window) = window else {
            eprintln!("SKIPPED: could not register for notifications");
            // Could not register (no notification service, e.g. under Wine).
            let result = watcher.await.unwrap();
            assert!(result.is_err(), "no window but no error either");
            return;
        };
        let window = window.0 as isize;
        let send = move |message: u32, wparam: usize| {
            tokio::task::spawn_blocking(move || {
                // SAFETY: a message to our own window from another thread.
                unsafe {
                    SendMessageW(
                        HWND(ptr::with_exposed_provenance_mut(window as usize)),
                        message,
                        Some(WPARAM(wparam)),
                        Some(LPARAM(1)),
                    );
                }
            })
        };
        send(WM_POWERBROADCAST, PBT_APMSUSPEND).await.unwrap();
        assert_eq!(events.recv().await, Some(SessionEvent::Sleeping));
        send(WM_WTSSESSION_CHANGE, 8).await.unwrap();
        send(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK).await.unwrap();
        assert_eq!(events.recv().await, Some(SessionEvent::ScreenLocked));

        drop(events);
        tokio::time::timeout(Duration::from_secs(5), watcher)
            .await
            .expect("watch ends once nobody listens")
            .unwrap()
            .unwrap();
    }
}
