//! `nibwarden-gui settings` (phase2.md §5.6): every change is written to config.toml at
//! once (keeping the rest of the file) and the daemon is asked to reload it.

use std::{path::PathBuf, process::ExitCode};

use nibwarden_config::{Ask, Config, Remember, Theme as ThemeSetting, Value};
use nibwarden_ipc::{Client, Request};
use slint::ComponentHandle as _;

use crate::{SettingsWindow, prompt};

/// The choices behind each segmented control, in display order.
const LANGUAGES: [&str; 4] = ["system", "en", "zh-TW", "zh-CN"];
const IDLE_MINUTES: [u32; 6] = [1, 5, 15, 30, 60, 0];
const CLEAR_SECONDS: [u32; 5] = [30, 60, 90, 120, 300];
const REMEMBER: [&str; 3] = ["request", "application", "terminal"];
/// "Until locked" (0), then hours, as `approval.remember_hours`.
const REMEMBER_HOURS: [u32; 4] = [0, 1, 4, 12];

/// Index of `value` in `choices`, or of the closest one.
fn closest(choices: &[u32], value: u32) -> i32 {
    let key = |c: &u32| if *c == 0 { u32::MAX } else { *c };
    let target = if value == 0 { u32::MAX } else { value };
    choices
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| key(c).abs_diff(target))
        .map_or(0, |(i, _)| i as i32)
}

fn language_index(language: &str) -> i32 {
    let normal = language.replace('_', "-").to_lowercase();
    LANGUAGES
        .iter()
        .position(|l| l.to_lowercase() == normal)
        .unwrap_or(0) as i32
}

pub fn run() -> ExitCode {
    let config = nibwarden_config::load();
    let ui = match SettingsWindow::new() {
        Ok(ui) => ui,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            return ExitCode::FAILURE;
        }
    };
    prompt::apply_appearance(&ui, &config.appearance);
    show(&ui, &config);
    connect(&ui);
    crate::developer::show(&ui);
    crate::developer::connect(&ui);
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
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            ExitCode::FAILURE
        }
    }
}

fn show(ui: &SettingsWindow, config: &Config) {
    ui.set_theme(match config.appearance.theme {
        ThemeSetting::System => 0,
        ThemeSetting::Light => 1,
        ThemeSetting::Dark => 2,
    });
    ui.set_language(language_index(&config.appearance.language));
    ui.set_idle(closest(&IDLE_MINUTES, config.lock.idle_minutes));
    ui.set_lock_on_screen_lock(config.lock.on_screen_lock);
    ui.set_lock_on_sleep(config.lock.on_sleep);
    ui.set_clear_clipboard(config.clipboard.clear);
    ui.set_clear_after(closest(
        &CLEAR_SECONDS,
        config.clipboard.clear_after_seconds,
    ));
    ui.set_ask_read(config.approval.read_secret == Ask::Ask);
    ui.set_remember(match config.approval.remember {
        Remember::Request => 0,
        Remember::Application => 1,
        Remember::Terminal => 2,
    });
    ui.set_remember_for(closest(&REMEMBER_HOURS, config.approval.remember_hours));
    ui.set_allow_without_window(config.approval.without_window == Ask::Allow);
    ui.set_tray(config.tray.enabled);
    show_browsers(ui);
}

/// The browser extension's host: `nibwarden-browser-host` beside this program.
fn browser_host() -> std::path::PathBuf {
    std::env::current_exe()
        .unwrap_or_default()
        .with_file_name("nibwarden-browser-host")
}

/// Whether the browsers start nibwarden for the Bitwarden extension, and which start
/// another program.
fn show_browsers(ui: &SettingsWindow) {
    use nibwarden_platform::native_messaging::{Registered, browsers};

    let browsers = browsers(&browser_host());
    ui.set_browser(
        browsers
            .iter()
            .any(|b| b.registered == Registered::Nibwarden),
    );
    let others: Vec<&str> = browsers
        .iter()
        .filter(|b| matches!(b.registered, Registered::Other(_)))
        .map(|b| b.name)
        .collect();
    ui.set_browser_others(others.join(", ").into());
}

/// Writes one setting, tells the daemon, and shows any problem in the window.
fn save(ui: &slint::Weak<SettingsWindow>, section: &str, key: &str, value: impl Into<Value>) {
    let error = match nibwarden_config::set(section, key, value) {
        Ok(()) => {
            reload_daemon();
            String::new()
        }
        Err(e) => format!("cannot save the settings: {e}"),
    };
    if let Some(ui) = ui.upgrade() {
        ui.set_error(error.into());
    }
}

/// Best effort: a daemon that is not running reads the file when it starts.
fn reload_daemon() {
    std::thread::spawn(|| {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            let socket = std::env::var_os("NIBWARDEN_SOCKET")
                .map(PathBuf::from)
                .or_else(nibwarden_ipc::default_socket_path);
            if let Some(socket) = socket
                && let Ok(mut client) = Client::connect(&socket, "nibwarden-gui").await
            {
                let _ = client.call::<()>(Request::ReloadConfig).await;
            }
        });
    });
}

fn ask(on: bool, off: Ask) -> &'static str {
    match if on { Ask::Ask } else { off } {
        Ask::Ask => "ask",
        Ask::Allow => "allow",
        Ask::Deny => "deny",
    }
}

fn connect(ui: &SettingsWindow) {
    let weak = ui.as_weak();
    ui.on_theme_chosen(move |index| {
        let theme = ["system", "light", "dark"][index.clamp(0, 2) as usize];
        save(&weak, "appearance", "theme", theme);
        reapply(&weak);
    });
    let weak = ui.as_weak();
    ui.on_language_chosen(move |index| {
        save(
            &weak,
            "appearance",
            "language",
            LANGUAGES[index.clamp(0, 3) as usize],
        );
        reapply(&weak);
    });
    let weak = ui.as_weak();
    ui.on_idle_chosen(move |index| {
        let minutes = IDLE_MINUTES[index.clamp(0, 5) as usize];
        save(&weak, "lock", "idle_minutes", i64::from(minutes));
    });
    let weak = ui.as_weak();
    ui.on_lock_on_screen_lock_changed(move |on| save(&weak, "lock", "on_screen_lock", on));
    let weak = ui.as_weak();
    ui.on_lock_on_sleep_changed(move |on| save(&weak, "lock", "on_sleep", on));
    let weak = ui.as_weak();
    ui.on_clear_clipboard_changed(move |on| save(&weak, "clipboard", "clear", on));
    let weak = ui.as_weak();
    ui.on_clear_after_chosen(move |index| {
        let seconds = CLEAR_SECONDS[index.clamp(0, 4) as usize];
        save(
            &weak,
            "clipboard",
            "clear_after_seconds",
            i64::from(seconds),
        );
    });
    let weak = ui.as_weak();
    ui.on_ask_read_changed(move |on| save(&weak, "approval", "read_secret", ask(on, Ask::Allow)));
    let weak = ui.as_weak();
    ui.on_remember_chosen(move |index| {
        save(
            &weak,
            "approval",
            "remember",
            REMEMBER[index.clamp(0, 2) as usize],
        );
    });
    let weak = ui.as_weak();
    ui.on_remember_for_chosen(move |index| {
        let hours = REMEMBER_HOURS[index.clamp(0, 3) as usize];
        save(&weak, "approval", "remember_hours", i64::from(hours));
    });
    let weak = ui.as_weak();
    ui.on_allow_without_window_changed(move |on| {
        let value = if on { "allow" } else { "deny" };
        save(&weak, "approval", "without_window", value);
    });
    let weak = ui.as_weak();
    ui.on_tray_changed(move |on| {
        save(&weak, "tray", "enabled", on);
        if let Err(e) = set_tray(on)
            && let Some(ui) = weak.upgrade()
        {
            ui.set_error(format!("cannot start the tray icon at login: {e}").into());
        }
    });
    let weak = ui.as_weak();
    ui.on_browser_changed(move |on| {
        let host = browser_host();
        let done = if on && !host.exists() {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} is missing; reinstall nibwarden", host.display()),
            ))
        } else if on {
            nibwarden_platform::native_messaging::register(&host, false).map(drop)
        } else {
            nibwarden_platform::native_messaging::unregister(&host)
        };
        if let Some(ui) = weak.upgrade() {
            if let Err(e) = done {
                ui.set_error(format!("cannot set up the browser extension: {e}").into());
            }
            show_browsers(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_install_shortcut(move || {
        let command = match std::env::current_exe() {
            Ok(exe) => format!("{} quick", exe.display()),
            Err(_) => "nibwarden-gui quick".to_owned(),
        };
        let status = match nibwarden_platform::shortcut::install(
            "nibwarden quick access",
            &command,
            "<Control><Shift>space",
        ) {
            Ok(installed) => format!("✓ {}", installed.location),
            Err(e) => e.to_string(),
        };
        if let Some(ui) = weak.upgrade() {
            ui.set_shortcut_status(status.into());
        }
    });
    let weak = ui.as_weak();
    ui.on_close_requested(move || {
        if let Some(ui) = weak.upgrade() {
            let _ = ui.hide();
        }
        let _ = slint::quit_event_loop();
    });
}

/// Starts the tray icon at login from now on, and right away; a running icon ends by
/// itself when the setting is turned off.
fn set_tray(on: bool) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let command = [
        exe.display().to_string(),
        "tray".to_owned(),
        crate::tray::AUTOSTART_ARG.to_owned(),
    ];
    nibwarden_platform::autostart::set(crate::tray::AUTOSTART, "nibwarden", &command, on)?;
    if on {
        let mut child = std::process::Command::new(exe).arg("tray").spawn()?;
        std::thread::spawn(move || child.wait());
    }
    Ok(())
}

/// Theme and language take effect in this window right away.
fn reapply(ui: &slint::Weak<SettingsWindow>) {
    if let Some(ui) = ui.upgrade() {
        prompt::apply_appearance(&ui, &nibwarden_config::load().appearance);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_values_to_choices() {
        assert_eq!(closest(&IDLE_MINUTES, 15), 2);
        assert_eq!(closest(&IDLE_MINUTES, 0), 5);
        assert_eq!(closest(&IDLE_MINUTES, 20), 2);
        assert_eq!(closest(&CLEAR_SECONDS, 90), 2);
        assert_eq!(closest(&REMEMBER_HOURS, 0), 0);
        assert_eq!(closest(&REMEMBER_HOURS, 4), 2);
        assert_eq!(closest(&REMEMBER_HOURS, 6), 2);
        assert_eq!(language_index("zh_TW"), 2);
        assert_eq!(language_index("ja"), 0);
    }
}
