//! `nibwarden-gui settings` (phase2.md §5.6): every change is written to config.toml at
//! once (keeping the rest of the file) and the daemon is asked to reload it.

use std::{path::PathBuf, process::ExitCode};

use nibwarden_config::{Ask, Config, Remember, RememberFor, Theme as ThemeSetting, Value};
use nibwarden_ipc::{Client, Request};
use slint::{ComponentHandle as _, SharedString};

use crate::{SettingsWindow, prompt};

/// The choices behind each segmented control, in display order.
const LANGUAGES: [&str; 4] = ["system", "en", "zh-TW", "zh-CN"];
const IDLE_MINUTES: [u32; 6] = [1, 5, 15, 30, 60, 0];
const CLEAR_SECONDS: [u32; 5] = [30, 60, 90, 120, 300];
const REMEMBER: [&str; 3] = ["request", "application", "terminal"];
/// `approval.remember_for`, like 1Password's "remember key approval".
const REMEMBER_FOR: [RememberFor; 5] = [
    RememberFor::Lock,
    RememberFor::Quit,
    RememberFor::Hours(4),
    RememberFor::Hours(12),
    RememberFor::Hours(24),
];

/// The choice shown for a `remember_for` from the file: other hours go to the closest.
fn remember_for_index(remember_for: RememberFor) -> i32 {
    match remember_for {
        RememberFor::Lock => 0,
        RememberFor::Quit => 1,
        RememberFor::Hours(hours) => 2 + closest(&[4, 12, 24], hours),
    }
}

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
    crate::unlock_settings::connect(&ui);
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
    ui.set_idle_action(match config.lock.idle_action {
        nibwarden_config::IdleAction::Lock => 0,
        nibwarden_config::IdleAction::Logout => 1,
    });
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
    ui.set_remember_for(remember_for_index(config.approval.remember_for));
    ui.set_allow_without_window(config.approval.without_window == Ask::Allow);
    ui.set_tray(config.tray.enabled);
    ui.set_minimize_after_copy(config.clipboard.minimize_after_copy);
    ui.set_website_icons(config.appearance.website_icons);
    ui.set_start_at_login(nibwarden_platform::service::starts_at_login());
    show_browsers(ui);
    show_shortcut(ui);
}

/// Quick access's shortcut, as the desktop has it.
fn show_shortcut(ui: &SettingsWindow) {
    ui.set_shortcut_supported(nibwarden_platform::shortcut::supported());
    let current = nibwarden_platform::shortcut::current().unwrap_or_default();
    ui.set_shortcut(readable(&current).into());
}

/// A key with its modifiers as a GTK accelerator (`<Control><Shift>space`); `None` for one
/// that cannot be a shortcut (a modifier alone, or a key without Ctrl, Alt or Super,
/// which typing would set off).
fn accelerator(text: &str, control: bool, alt: bool, shift: bool, meta: bool) -> Option<String> {
    use slint::platform::Key;

    let c = text.chars().next()?;
    let named = [
        (Key::Space, "space"),
        (Key::Return, "Return"),
        (Key::Tab, "Tab"),
        (Key::Insert, "Insert"),
        (Key::Home, "Home"),
        (Key::End, "End"),
        (Key::PageUp, "Page_Up"),
        (Key::PageDown, "Page_Down"),
        (Key::UpArrow, "Up"),
        (Key::DownArrow, "Down"),
        (Key::LeftArrow, "Left"),
        (Key::RightArrow, "Right"),
    ];
    let function = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ]
    .iter()
    .position(|k| char::from(*k) == c);
    let key = if let Some(n) = function {
        format!("F{}", n + 1)
    } else if let Some((_, name)) = named.iter().find(|(k, _)| char::from(*k) == c) {
        (*name).to_owned()
    } else if text.chars().count() == 1 && c.is_ascii_alphanumeric() {
        c.to_ascii_lowercase().to_string()
    } else {
        return None;
    };
    if function.is_none() && !(control || alt || meta) {
        return None;
    }
    let mut binding = String::new();
    for (on, name) in [
        (control, "<Control>"),
        (alt, "<Alt>"),
        (shift, "<Shift>"),
        (meta, "<Super>"),
    ] {
        if on {
            binding.push_str(name);
        }
    }
    Some(binding + &key)
}

/// A GTK accelerator as people write it: `<Control><Shift>space` → `Ctrl+Shift+Space`.
fn readable(binding: &str) -> String {
    let mut parts = Vec::new();
    let mut rest = binding;
    while let Some(after) = rest.strip_prefix('<') {
        let Some((modifier, tail)) = after.split_once('>') else {
            break;
        };
        parts.push(match modifier.to_ascii_lowercase().as_str() {
            "control" | "primary" | "ctrl" => "Ctrl".to_owned(),
            "alt" => "Alt".to_owned(),
            "shift" => "Shift".to_owned(),
            "super" | "meta" => "Super".to_owned(),
            other => other.to_owned(),
        });
        rest = tail;
    }
    if !rest.is_empty() {
        let key = rest.replace('_', " ");
        let mut chars = key.chars();
        parts.push(match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => key,
        });
    }
    parts.join("+")
}

/// The browser extension's host: `nibwarden-browser-host` beside this program.
fn browser_host() -> std::path::PathBuf {
    std::env::current_exe()
        .unwrap_or_default()
        .with_file_name(format!(
            "nibwarden-browser-host{}",
            std::env::consts::EXE_SUFFIX
        ))
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
pub(crate) fn reload_daemon() {
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
        let value = match REMEMBER_FOR[index.clamp(0, 4) as usize] {
            RememberFor::Lock => Value::from("lock"),
            RememberFor::Quit => Value::from("quit"),
            RememberFor::Hours(hours) => Value::from(i64::from(hours)),
        };
        save(&weak, "approval", "remember_for", value);
    });
    let weak = ui.as_weak();
    ui.on_allow_without_window_changed(move |on| {
        let value = if on { "allow" } else { "deny" };
        save(&weak, "approval", "without_window", value);
    });
    let weak = ui.as_weak();
    ui.on_tray_changed(move |on| {
        save(&weak, "tray", "enabled", on);
        if let Err(e) = crate::tray::set(on)
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
    ui.on_idle_action_chosen(move |index| {
        let action = if index == 1 { "logout" } else { "lock" };
        save(&weak, "lock", "idle_action", action);
    });
    let weak = ui.as_weak();
    ui.on_website_icons_changed(move |on| {
        save(&weak, "appearance", "website_icons", on);
    });
    let weak = ui.as_weak();
    ui.on_minimize_after_copy_changed(move |on| {
        save(&weak, "clipboard", "minimize_after_copy", on);
    });
    let weak = ui.as_weak();
    ui.on_start_at_login_changed(move |on| {
        let result = nibwarden_platform::service::set_start_at_login(on);
        if let Some(ui) = weak.upgrade() {
            if let Err(e) = result {
                ui.set_error(format!("cannot change starting at login: {e}").into());
            }
            ui.set_start_at_login(nibwarden_platform::service::starts_at_login());
        }
    });
    let weak = ui.as_weak();
    ui.on_shortcut_pressed(move |text, control, alt, shift, meta| {
        let Some(binding) = accelerator(&text, control, alt, shift, meta) else {
            return false;
        };
        let command = match std::env::current_exe() {
            Ok(exe) => format!("{} quick", exe.display()),
            Err(_) => "nibwarden-gui quick".to_owned(),
        };
        let result =
            nibwarden_platform::shortcut::install("nibwarden quick access", &command, &binding)
                .and_then(|installed| crate::hotkey::set(true).map(|()| installed));
        if let Some(ui) = weak.upgrade() {
            ui.set_shortcut_status(match result {
                Ok(_) => SharedString::new(),
                Err(e) => e.to_string().into(),
            });
            show_shortcut(&ui);
        }
        true
    });
    let weak = ui.as_weak();
    ui.on_shortcut_cleared(move || {
        let result =
            nibwarden_platform::shortcut::remove().and_then(|()| crate::hotkey::set(false));
        if let Some(ui) = weak.upgrade() {
            ui.set_shortcut_status(match result {
                Ok(()) => SharedString::new(),
                Err(e) => e.to_string().into(),
            });
            show_shortcut(&ui);
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
    fn shortcuts() {
        let space = char::from(slint::platform::Key::Space).to_string();
        let f5 = char::from(slint::platform::Key::F5).to_string();
        let shift = char::from(slint::platform::Key::Shift).to_string();
        assert_eq!(
            accelerator(&space, true, false, true, false).as_deref(),
            Some("<Control><Shift>space")
        );
        assert_eq!(
            accelerator("P", false, true, true, false).as_deref(),
            Some("<Alt><Shift>p")
        );
        assert_eq!(
            accelerator(&f5, false, false, false, false).as_deref(),
            Some("F5")
        );
        // Typing would set these off, or they are no key at all.
        assert_eq!(accelerator("p", false, false, true, false), None);
        assert_eq!(accelerator(&shift, true, false, true, false), None);
        assert_eq!(accelerator("!", true, false, true, false), None);
        assert_eq!(readable("<Control><Shift>space"), "Ctrl+Shift+Space");
        assert_eq!(readable("<Super>Page_Up"), "Super+Page Up");
        assert_eq!(readable(""), "");
    }

    #[test]
    fn maps_values_to_choices() {
        assert_eq!(closest(&IDLE_MINUTES, 15), 2);
        assert_eq!(closest(&IDLE_MINUTES, 0), 5);
        assert_eq!(closest(&IDLE_MINUTES, 20), 2);
        assert_eq!(closest(&CLEAR_SECONDS, 90), 2);
        assert_eq!(remember_for_index(RememberFor::Lock), 0);
        assert_eq!(remember_for_index(RememberFor::Quit), 1);
        assert_eq!(remember_for_index(RememberFor::Hours(4)), 2);
        assert_eq!(remember_for_index(RememberFor::Hours(1)), 2);
        assert_eq!(remember_for_index(RememberFor::Hours(24)), 4);
        assert_eq!(language_index("zh_TW"), 2);
        assert_eq!(language_index("ja"), 0);
    }
}
