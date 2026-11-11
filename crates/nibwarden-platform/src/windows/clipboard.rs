// Not ported yet: reports Unsupported like other platforms.
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
