//! Checks the clipboard on this desktop (phase2.md Q5): sets a test value as a secret,
//! shows what another program reads, then clears it.
//!
//!     cargo run -p nibwarden-platform --features clipboard --example clipboard-check

use std::{process::Command, thread::sleep, time::Duration};

use nibwarden_platform::clipboard::Clipboard;

fn read_with_xclip() -> String {
    Command::new("xclip")
        .args(["-o", "-selection", "clipboard"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_else(|e| format!("(xclip failed: {e})"))
}

fn main() {
    let value = format!("nibwarden-check-{}", std::process::id());
    let mut clipboard = Clipboard::open().expect("open clipboard");
    clipboard.set_secret(&value).expect("set");
    sleep(Duration::from_millis(300));
    println!("set:     {value}");
    println!("xclip:   {}", read_with_xclip());
    println!(
        "cleared: {}",
        clipboard.clear_if_still(&value).expect("clear")
    );
    sleep(Duration::from_millis(300));
    println!("after:   {:?}", read_with_xclip());
}
