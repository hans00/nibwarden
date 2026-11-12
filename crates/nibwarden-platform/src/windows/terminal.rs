//! The console, whatever stdin and stdout are redirected to: `CONIN$` and `CONOUT$`.
//! `rpassword` (which `read_password` uses) reads `CONIN$` with echo off and prints
//! to `CONOUT$` by itself.

use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::windows::io::AsRawHandle as _,
};

use crate::winutil::IoResult as _;
use windows::Win32::{
    Foundation::HANDLE,
    System::Console::{ReadConsoleW, WriteConsoleW},
};

pub struct Console {
    input: File,
    output: File,
    /// Bytes of a line already read but not yet handed out.
    pending: Vec<u8>,
}

pub fn open() -> io::Result<Console> {
    // Both fail without a console (a service, a detached process).
    let open = |name: &str| OpenOptions::new().read(true).write(true).open(name);
    Ok(Console {
        input: open("CONIN$")?,
        output: open("CONOUT$")?,
        pending: Vec::new(),
    })
}

impl Read for Console {
    /// One line at a time, as UTF-8 (the console's code page would mangle anything that
    /// is not ASCII, so this reads UTF-16).
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            let mut wide = [0u16; 512];
            let mut read = 0u32;
            // SAFETY: `wide` and `read` outlive the call; no input control.
            unsafe {
                ReadConsoleW(
                    HANDLE(self.input.as_raw_handle()),
                    wide.as_mut_ptr().cast(),
                    wide.len() as u32,
                    &mut read,
                    None,
                )
                .io()?;
            }
            // A split surrogate pair would become U+FFFD; a line of over 512 UTF-16 units
            // is not a code or a password anyone types.
            self.pending = String::from_utf16_lossy(&wide[..read as usize]).into_bytes();
            self.pending.reverse();
        }
        let mut n = 0;
        while n < buf.len() {
            let Some(byte) = self.pending.pop() else {
                break;
            };
            buf[n] = byte;
            n += 1;
        }
        Ok(n)
    }
}

impl Write for Console {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text: Vec<u16> = String::from_utf8_lossy(buf).encode_utf16().collect();
        let mut written = 0u32;
        // SAFETY: `text` and `written` outlive the call.
        unsafe {
            WriteConsoleW(
                HANDLE(self.output.as_raw_handle()),
                &text,
                Some(&mut written),
                None,
            )
            .io()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
