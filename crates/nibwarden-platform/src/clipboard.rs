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

#[cfg(target_os = "linux")]
mod imp {
    use std::io;

    use arboard::SetExtLinux as _;

    pub struct Clipboard(arboard::Clipboard);

    fn io_error(e: arboard::Error) -> io::Error {
        io::Error::other(e.to_string())
    }

    impl Clipboard {
        pub fn open() -> io::Result<Self> {
            arboard::Clipboard::new().map(Self).map_err(io_error)
        }

        pub fn set_secret(&mut self, text: &str) -> io::Result<()> {
            self.0
                .set()
                .exclude_from_history()
                .text(text)
                .map_err(io_error)
        }

        pub fn still_holds(&mut self, text: &str) -> bool {
            self.0.get_text().is_ok_and(|current| current == text)
        }

        pub fn clear_if_still(&mut self, text: &str) -> io::Result<bool> {
            if self.still_holds(text) {
                self.0.clear().map_err(io_error)?;
                return Ok(true);
            }
            Ok(false)
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::io;

    pub enum Clipboard {}

    impl Clipboard {
        pub fn open() -> io::Result<Self> {
            Err(crate::unsupported())
        }

        pub fn set_secret(&mut self, _: &str) -> io::Result<()> {
            match *self {}
        }

        pub fn clear_if_still(&mut self, _: &str) -> io::Result<bool> {
            match *self {}
        }

        pub fn still_holds(&mut self, _: &str) -> bool {
            match *self {}
        }
    }
}
