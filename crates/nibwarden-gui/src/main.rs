//! `nibwarden-gui`: windows for nibwardend (phase2.md §5). `prompt` is started by the
//! daemon with the prompt protocol on stdin/stdout (phase2.md E3) and exits as soon as
//! the prompt is answered, so a typed master password does not linger in this process.

use std::{process::ExitCode, time::Instant};

fn main() -> ExitCode {
    let started = Instant::now();
    let mode = std::env::args().nth(1);
    // The tray icon and the shortcut's holder have no window, so no window system either.
    if !matches!(mode.as_deref(), Some("tray" | "shortcut")) {
        nibwarden_gui::appearance::select_renderer();
    }
    if matches!(mode.as_deref(), Some("vault" | "quick" | "settings") | None) {
        nibwarden_gui::tray::restore();
        nibwarden_gui::hotkey::restore();
    }
    match mode.as_deref() {
        Some("prompt") => nibwarden_gui::prompt::run(started),
        Some("quick") => nibwarden_gui::quick::run(),
        Some("settings") => nibwarden_gui::settings::run(),
        Some("tray") => nibwarden_gui::tray::run(
            std::env::args().nth(2).as_deref() == Some(nibwarden_gui::tray::AUTOSTART_ARG),
        ),
        Some("shortcut") => nibwarden_gui::hotkey::run(),
        Some("vault") | None => nibwarden_gui::vault_window::run(started),
        _ => {
            eprintln!("usage: nibwarden-gui [vault]    the main window");
            eprintln!("       nibwarden-gui quick      quick access to the vault");
            eprintln!("       nibwarden-gui settings   settings");
            eprintln!("       nibwarden-gui tray       the system tray icon");
            eprintln!(
                "       nibwarden-gui shortcut   holds the quick access shortcut (macOS, Windows)"
            );
            eprintln!("       nibwarden-gui prompt     (started by nibwardend)");
            ExitCode::from(2)
        }
    }
}
