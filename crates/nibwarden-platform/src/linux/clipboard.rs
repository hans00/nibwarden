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
