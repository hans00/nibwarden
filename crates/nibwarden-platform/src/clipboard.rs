//! Putting a secret on the clipboard (phase2.md §7), for the quick access window. Only
//! compiled with the `clipboard` feature, so the daemon does not link a clipboard stack.

use std::io;

/// The system clipboard. On Linux the owning process must stay alive for the content to
/// remain available (X11 and Wayland both serve the clipboard from the program that set
/// it), so keep this until the content has been cleared.
pub struct Clipboard(imp::Clipboard);

impl Clipboard {
    pub fn open() -> io::Result<Self> {
        imp::Clipboard::open().map(Self)
    }

    /// Sets `text`, marked so clipboard history tools skip it where the desktop supports
    /// such a marker (KDE's `x-kde-passwordManagerHint`).
    pub fn set_secret(&mut self, text: &str) -> io::Result<()> {
        self.0.set_secret(text)
    }

    /// Clears the clipboard if it still holds `text`, i.e. nobody copied anything since.
    pub fn clear_if_still(&mut self, text: &str) -> io::Result<bool> {
        self.0.clear_if_still(text)
    }

    /// Whether the clipboard still holds `text`.
    pub fn still_holds(&mut self, text: &str) -> bool {
        self.0.still_holds(text)
    }
}

#[cfg_attr(target_os = "linux", path = "linux/clipboard.rs")]
#[cfg_attr(target_os = "macos", path = "macos/clipboard.rs")]
#[cfg_attr(windows, path = "windows/clipboard.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/clipboard.rs"
)]
mod imp;
