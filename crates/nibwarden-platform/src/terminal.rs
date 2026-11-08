//! The controlling terminal: where interactive prompts go even when stdin and stdout are
//! redirected (stdin may be carrying a password, stdout a secret).

use std::io;

use zeroize::Zeroizing;

/// Whether a controlling terminal can be opened.
pub fn available() -> bool {
    imp::open().is_ok()
}

/// Reads a password from the terminal without echo.
pub fn read_password(prompt: &str) -> io::Result<Zeroizing<String>> {
    imp::open()?;
    Ok(Zeroizing::new(rpassword::prompt_password(prompt)?))
}

/// Reads a visible line from the terminal, trimmed.
pub fn read_line(prompt: &str) -> io::Result<String> {
    use std::io::{BufRead as _, Write as _};

    let mut tty = imp::open()?;
    tty.write_all(prompt.as_bytes())?;
    let mut line = String::new();
    io::BufReader::new(tty).read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{fs::File, io};

    pub fn open() -> io::Result<File> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::{fs::File, io};

    pub fn open() -> io::Result<File> {
        Err(crate::unsupported())
    }
}
