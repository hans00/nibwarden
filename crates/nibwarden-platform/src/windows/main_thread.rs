use std::{
    ptr,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::Duration,
};

use windows_sys::Win32::{
    System::Threading::{GetCurrentThreadId, INFINITE},
    UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_NOREMOVE,
        PM_REMOVE, PeekMessageW, PostThreadMessageW, QS_ALLINPUT, TranslateMessage, WM_APP,
    },
};

/// The thread [`wait`] runs on, once it has a message queue; 0 before.
static THREAD: AtomicU32 = AtomicU32::new(0);
/// A wake that came before the thread was known, or while it was not waiting.
static WOKEN: AtomicBool = AtomicBool::new(false);
const WM_WAKE: u32 = WM_APP + 0x4e57;

pub fn wait(timeout: Option<Duration>) {
    // SAFETY: MSG is plain data; PeekMessageW only writes into it.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: as above. The first call gives this thread its message queue.
    unsafe { PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_NOREMOVE) };
    // SAFETY: no failure mode.
    THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    if !WOKEN.swap(false, Ordering::SeqCst) {
        let millis = timeout.map_or(INFINITE, |t| {
            u32::try_from(t.as_millis())
                .unwrap_or(INFINITE - 1)
                .min(INFINITE - 1)
        });
        // SAFETY: no handles to wait on, only this thread's input.
        unsafe {
            MsgWaitForMultipleObjectsEx(0, ptr::null(), millis, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
    }
    WOKEN.store(false, Ordering::SeqCst);
    // SAFETY: standard message pump on this thread's own queue.
    while unsafe { PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
        if msg.hwnd.is_null() && msg.message == WM_WAKE {
            continue;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

pub fn wake() {
    WOKEN.store(true, Ordering::SeqCst);
    let thread = THREAD.load(Ordering::SeqCst);
    if thread != 0 {
        // SAFETY: posting to a thread's queue; fails harmlessly if the thread is gone.
        unsafe { PostThreadMessageW(thread, WM_WAKE, 0, 0) };
    }
}
