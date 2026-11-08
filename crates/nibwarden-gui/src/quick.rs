//! `nibwarden-gui quick`: quick access (phase2.md §7), a small window to search the vault
//! and copy from it, opened with a global shortcut. The vault part is `vault_ui`.

use std::process::ExitCode;

use nibwarden_platform::paths;
use slint::ComponentHandle as _;

use crate::{QuickWindow, Vault, prompt, vault_ui};

pub fn run() -> ExitCode {
    // One quick access window at a time; a second shortcut press does nothing.
    let lock = paths::runtime_dir().and_then(|dir| {
        nibwarden_platform::fs::create_private_dir(&dir).ok()?;
        nibwarden_platform::fs::try_lock(&dir.join("quick.lock")).ok()
    });
    let Some(Some(_lock)) = lock else {
        return ExitCode::SUCCESS;
    };
    let settings = nibwarden_config::load();
    let ui = match QuickWindow::new() {
        Ok(ui) => ui,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            return ExitCode::FAILURE;
        }
    };
    prompt::apply_appearance(&ui, &settings.appearance);
    let attached = vault_ui::attach(&ui, &settings);
    let weak = ui.as_weak();
    ui.global::<Vault>().on_close(move || {
        // Hide first: the event loop stopping alone would leave a frozen window on screen.
        if let Some(ui) = weak.upgrade() {
            let _ = ui.hide();
        }
        let _ = slint::quit_event_loop();
    });
    let weak = ui.as_weak();
    ui.global::<Vault>().on_open_main(move || {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("vault").spawn();
        }
        if let Some(ui) = weak.upgrade() {
            ui.global::<Vault>().invoke_close();
        }
    });
    // Follow theme and language changes made in the settings window.
    let _follow = {
        let weak = ui.as_weak();
        crate::appearance::follow_settings(move |appearance| {
            if let Some(ui) = weak.upgrade() {
                prompt::apply_appearance(&ui, appearance);
            }
        })
    };
    let result = ui.run();
    drop(ui);
    attached.finish();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            ExitCode::FAILURE
        }
    }
}
