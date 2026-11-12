use std::{
    io,
    process::{Command, Stdio},
};

/// `open <url>`: LaunchServices picks the default browser. `url` always starts with
/// `http://` or `https://` (the interface checked), so it cannot be taken for an option.
pub fn open(url: &str) -> io::Result<()> {
    let mut child = Command::new("/usr/bin/open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reap it without blocking the caller; `open` exits once LaunchServices has the URL.
    std::thread::spawn(move || child.wait());
    Ok(())
}
