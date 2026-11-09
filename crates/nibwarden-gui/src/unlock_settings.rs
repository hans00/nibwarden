//! The settings window's "Unlock" section (phase2.md §12.19): unlocking with the system's
//! authentication, and with a PIN, once the master password unlocked since the daemon
//! started.

use nibwarden_ipc::{Request, StatusResult, UnlockWith};
use slint::{ComponentHandle as _, SharedString};
use zeroize::Zeroizing;

use crate::{
    SettingsWindow,
    developer::{block_on, connect_daemon},
};

pub fn connect(ui: &SettingsWindow) {
    ui.set_system_unlock(nibwarden_config::load().unlock.system);
    // Whether a PIN is set, from the daemon.
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let status = block_on(async {
            let mut client = connect_daemon().await?;
            client
                .call::<StatusResult>(Request::Status)
                .await
                .map_err(|e| e.to_string())
        });
        if let Ok(Some(status)) = status {
            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_pin_unlock(status.pin_set));
        }
    });

    let weak = ui.as_weak();
    ui.on_system_unlock_changed(move |on| {
        let weak = weak.clone();
        if !on {
            if let Some(ui) = weak.upgrade() {
                ui.set_system_unlock_failed(false);
            }
            save_system(false);
            return;
        }
        // Not without the polkit action, which only a package installs: the switch goes
        // back off.
        std::thread::spawn(move || {
            let available = block_on(async { Ok(nibwarden_platform::polkit::available().await) })
                .ok()
                .flatten()
                .unwrap_or(false);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_system_unlock_failed(!available);
                ui.set_system_unlock(available);
                if available {
                    save_system(true);
                }
            });
        });
    });

    let weak = ui.as_weak();
    ui.on_pin_unlock_changed(move |on| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_pin_failed(false);
        ui.set_pin_text(SharedString::new());
        if on {
            // Set once the PIN is typed; until then nothing changes.
            ui.set_pin_entry(true);
            return;
        }
        ui.set_pin_entry(false);
        set_pin(&ui, None);
    });

    let weak = ui.as_weak();
    ui.on_pin_submitted(move || {
        let Some(ui) = weak.upgrade() else { return };
        let pin = Zeroizing::new(ui.get_pin_text().to_string());
        if pin.is_empty() {
            return;
        }
        ui.set_pin_text(SharedString::new());
        set_pin(&ui, Some(pin));
    });
}

fn save_system(on: bool) {
    if let Err(e) = nibwarden_config::set("unlock", "system", on) {
        tracing::warn!(error = %e, "cannot save the unlock setting");
    }
    crate::settings::reload_daemon();
}

/// Sets or removes the PIN in the background; setting it needs the vault unlocked, so the
/// unlock window may open.
fn set_pin(ui: &SettingsWindow, pin: Option<Zeroizing<String>>) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let setting = pin.is_some();
        let done = block_on(async move {
            let mut client = connect_daemon().await?;
            client
                .call_unlocking::<()>(Request::SetPin { pin }, UnlockWith::Window)
                .await
                .map_err(|e| e.to_string())
        });
        let ok = matches!(done, Ok(Some(())));
        if let Err(e) = &done {
            tracing::warn!(error = %e, "cannot change the PIN");
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_pin_failed(!ok);
            if setting {
                ui.set_pin_entry(!ok);
                ui.set_pin_unlock(ok);
            } else if !ok {
                ui.set_pin_unlock(true);
            }
        });
    });
}
