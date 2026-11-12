use std::{
    io,
    process::{Command, Stdio},
};

pub fn open(url: &str) -> io::Result<()> {
    let mut child = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reap it without blocking the caller; xdg-open exits once the browser has it.
    std::thread::spawn(move || child.wait());
    Ok(())
}
