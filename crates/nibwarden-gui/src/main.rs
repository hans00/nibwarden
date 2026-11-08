//! `nibwarden-gui`: windows for nibwardend (phase2.md §5). `prompt` is started by the
//! daemon with the prompt protocol on stdin/stdout (phase2.md E3) and exits as soon as
//! the prompt is answered, so a typed master password does not linger in this process.

use std::{process::ExitCode, time::Instant};

fn main() -> ExitCode {
    let started = Instant::now();
    nibwarden_gui::appearance::select_renderer();
    match std::env::args().nth(1).as_deref() {
        Some("prompt") => nibwarden_gui::prompt::run(started),
        Some("quick") => nibwarden_gui::quick::run(),
        Some("settings") => nibwarden_gui::settings::run(),
        Some("tray") => nibwarden_gui::tray::run(
            std::env::args().nth(2).as_deref() == Some(nibwarden_gui::tray::AUTOSTART_ARG),
        ),
        Some("vault") | None => nibwarden_gui::vault_window::run(),
        _ => {
            eprintln!("usage: nibwarden-gui [vault]    the main window");
            eprintln!("       nibwarden-gui quick      quick access to the vault");
            eprintln!("       nibwarden-gui settings   settings");
            eprintln!("       nibwarden-gui tray       the system tray icon");
            eprintln!("       nibwarden-gui prompt     (started by nibwardend)");
            ExitCode::from(2)
        }
    }
}
