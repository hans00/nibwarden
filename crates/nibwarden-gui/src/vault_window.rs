//! `nibwarden-gui vault`: the main window (phase2.md P2-M7). Browse the whole vault by
//! category and folder, see and copy an item's fields, lock, sync, and open the settings.
//! The vault part is shared with quick access (`vault_ui`); this file adds the window
//! frame (it is frameless, like every nibwarden window) and following the daemon while
//! the window stays open.

use std::{process::ExitCode, time::Duration};

use nibwarden_platform::paths;
use slint::ComponentHandle as _;

use crate::{Vault, VaultWindow, prompt, vault_ui};

/// How often the window checks whether the vault locked or changed meanwhile (idle lock,
/// a sync, `nibwarden lock` in a terminal). A local socket round trip.
const REFRESH: Duration = Duration::from_secs(3);

pub fn run(started: std::time::Instant) -> ExitCode {
    // One main window; opening it again while it runs does nothing.
    let lock = paths::runtime_dir().and_then(|dir| {
        nibwarden_platform::fs::create_private_dir(&dir).ok()?;
        nibwarden_platform::fs::try_lock(&dir.join("vault-window.lock")).ok()
    });
    let Some(Some(_lock)) = lock else {
        return ExitCode::SUCCESS;
    };
    let settings = nibwarden_config::load();
    let ui = match VaultWindow::new() {
        Ok(ui) => ui,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            return ExitCode::FAILURE;
        }
    };
    crate::appearance::report_slow_start(ui.window(), started, started.elapsed());
    prompt::apply_appearance(&ui, &settings.appearance);
    let attached = vault_ui::attach(&ui, &settings);
    connect_frame(&ui);
    connect_drops(&ui, attached.dropper());
    crate::sends::attach(&ui, attached.copier());
    crate::transfer::attach(&ui, attached.refresher());
    crate::account::attach(&ui, attached.refresher());

    let refresher = slint::Timer::default();
    {
        refresher.start(slint::TimerMode::Repeated, REFRESH, attached.refresher());
    }
    let _follow = {
        let weak = ui.as_weak();
        crate::appearance::follow_settings(move |appearance| {
            if let Some(ui) = weak.upgrade() {
                prompt::apply_appearance(&ui, appearance);
            }
        })
    };
    let result = ui.run();
    drop(refresher);
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

/// Files dropped on the window go to the edit form. winit has them on X11, Windows and
/// macOS; on Wayland it has no drag and drop, so there only the file buttons work.
fn connect_drops(ui: &VaultWindow, dropped: impl Fn(std::path::PathBuf) + 'static) {
    use slint::winit_030::{EventResult, WinitWindowAccessor as _, winit::event::WindowEvent};

    ui.window().on_winit_window_event(move |_, event| {
        if let WindowEvent::DroppedFile(path) = event {
            dropped(path.clone());
            return EventResult::PreventDefault;
        }
        EventResult::Propagate
    });
}

fn connect_frame(ui: &VaultWindow) {
    let vault = ui.global::<Vault>();
    let weak = ui.as_weak();
    vault.on_close(move || {
        if let Some(ui) = weak.upgrade() {
            let _ = ui.hide();
        }
        let _ = slint::quit_event_loop();
    });
    vault.on_open_settings(|| {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("settings").spawn();
        }
    });

    let weak = ui.as_weak();
    vault.on_minimize_window(move || {
        if let Some(ui) = weak.upgrade() {
            ui.window().set_minimized(true);
        }
    });
    let weak = ui.as_weak();
    ui.on_minimize(move || {
        if let Some(ui) = weak.upgrade() {
            ui.window().set_minimized(true);
        }
    });
    let weak = ui.as_weak();
    ui.on_maximize(move || {
        if let Some(ui) = weak.upgrade() {
            let maximized = !ui.window().is_maximized();
            ui.window().set_maximized(maximized);
            ui.set_is_maximized(maximized);
        }
    });
}
