//! The user's graphical login session, as CoreGraphics reports it (`CGSession.h`).

use std::ffi::c_void;

use core_foundation::{base::TCFType as _, string::CFString};
use core_foundation_sys::{
    base::{CFGetTypeID, CFRelease, CFTypeRef},
    dictionary::{CFDictionaryGetValue, CFDictionaryRef},
    number::{CFBooleanGetTypeID, CFBooleanGetValue, CFNumberGetTypeID, CFNumberGetValue},
    number::{CFBooleanRef, CFNumberRef, kCFNumberSInt64Type},
};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// The dictionary of the calling process's login session, or NULL if it has none:
    /// a process started over SSH, or by a daemon, runs outside the user's GUI session.
    /// Follows the Create rule.
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

/// A reference to the session dictionary, released on drop.
struct Session(CFDictionaryRef);

impl Session {
    fn current() -> Option<Self> {
        // SAFETY: no arguments; the result is NULL or an owned dictionary.
        let dict = unsafe { CGSessionCopyCurrentDictionary() };
        (!dict.is_null()).then_some(Self(dict))
    }

    /// A boolean-like entry: the key is not documented to hold a `CFBoolean` or a
    /// `CFNumber`, so both are read.
    fn flag(&self, key: &str) -> Option<bool> {
        let key = CFString::new(key);
        // SAFETY: the dictionary is alive; the value is borrowed (Get rule) and only
        // inspected while `self` is.
        unsafe {
            let value: CFTypeRef =
                CFDictionaryGetValue(self.0, key.as_concrete_TypeRef().cast::<c_void>());
            if value.is_null() {
                return None;
            }
            let kind = CFGetTypeID(value);
            if kind == CFBooleanGetTypeID() {
                return Some(CFBooleanGetValue(value as CFBooleanRef));
            }
            if kind == CFNumberGetTypeID() {
                let mut number: i64 = 0;
                let ok = CFNumberGetValue(
                    value as CFNumberRef,
                    kCFNumberSInt64Type,
                    (&raw mut number).cast(),
                );
                return ok.then_some(number != 0);
            }
            None
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: we own one reference.
        unsafe { CFRelease(self.0.cast()) };
    }
}

/// Whether this process belongs to a graphical login session (Aqua), where windows can
/// appear. False over SSH without one, and in a background service.
pub fn exists() -> bool {
    Session::current().is_some()
}

/// Whether the session's screen is locked (`CGSSessionScreenIsLocked`, which the login
/// window sets while the lock screen is up; the screen saver alone does not set it).
pub fn screen_locked() -> bool {
    Session::current()
        .and_then(|s| s.flag("CGSSessionScreenIsLocked"))
        .unwrap_or(false)
}
