//! The user's theme and language settings (phase2.md §5.5) applied to the windows.

use nibwarden_config::{Appearance, Theme};

/// Translations compiled into the binary (`lang/<tag>/LC_MESSAGES`). English is the
/// source language and needs none.
pub const TRANSLATIONS: &[&str] = &["zh_TW", "zh_CN"];

/// Picks the translation for the most preferred language we have; `en` if none matches.
pub fn choose_language(preferred: &[String]) -> &'static str {
    for tag in preferred {
        if let Some(found) = TRANSLATIONS.iter().find(|t| **t == tag) {
            return found;
        }
        let (lang, region) = tag.split_once('_').unwrap_or((tag, ""));
        if lang == "en" {
            return "en";
        }
        if lang == "zh" {
            // Traditional script regions; everything else (CN, SG, bare zh) is Simplified.
            return if matches!(region, "TW" | "HK" | "MO") {
                "zh_TW"
            } else {
                "zh_CN"
            };
        }
        if let Some(found) = TRANSLATIONS
            .iter()
            .find(|t| t.split('_').next() == Some(lang))
        {
            return found;
        }
    }
    "en"
}

/// The desktop entry (packaging/desktop/nibwarden.desktop): Wayland app id, X11 class.
const APP_ID: &str = "nibwarden";

/// OpenGL (femtovg) first: rounded, shadowed windows need a transparent surface, which
/// the software renderer cannot give on Wayland (its buffers have no alpha). Software
/// rendering remains the fallback without OpenGL. `SLINT_BACKEND` still overrides.
/// Every window gets the app id `nibwarden`, so the desktop shows the entry's icon.
pub fn select_renderer() {
    if std::env::var_os("SLINT_BACKEND").is_some() {
        // Chosen by the variable; selected now, as the app id needs a platform. A failure
        // is said here: the window would only report that winit cannot start again.
        match slint::BackendSelector::new().select() {
            Ok(()) => drop(slint::set_xdg_app_id(APP_ID)),
            Err(e) => eprintln!("nibwarden-gui: cannot open windows: {e}"),
        }
        return;
    }
    // Choosing femtovg succeeds even without a driver; it fails only when its window
    // opens, and then there is no window. So the driver is asked first.
    let renderer = if nibwarden_platform::display::opengl_available() {
        "femtovg"
    } else {
        "software"
    };
    match slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.into())
        .select()
    {
        Ok(()) => drop(slint::set_xdg_app_id(APP_ID)),
        // The failed attempt used up winit's one event loop per process, so the software
        // renderer needs a fresh process.
        Err(e) if renderer == "femtovg" => restart_in_software(&e.to_string()),
        Err(e) => eprintln!("nibwarden-gui: cannot open windows: {e}"),
    }
}

/// Runs this program again with the software renderer (exec keeps the prompt pipes on
/// stdin/stdout).
fn restart_in_software(why: &str) {
    tracing::debug!(why, "restarting with the software renderer");
    if let Ok(exe) = std::env::current_exe() {
        let mut command = std::process::Command::new(exe);
        command
            .args(std::env::args_os().skip(1))
            .env("SLINT_BACKEND", "winit-software");
        let e = nibwarden_platform::process::exec(&mut command);
        eprintln!("nibwarden-gui: cannot restart with the software renderer: {e}");
    }
}

/// Re-applies theme and language whenever `config.toml` changes, so open windows follow
/// the settings window right away. Keep the returned timer alive.
pub fn follow_settings(apply: impl Fn(&Appearance) + 'static) -> slint::Timer {
    let modified = || {
        nibwarden_config::path()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
    };
    let last = std::cell::Cell::new(modified());
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_secs(1),
        move || {
            let now = modified();
            if now != last.get() {
                last.set(now);
                apply(&nibwarden_config::load().appearance);
            }
        },
    );
    timer
}

/// Switches the bundled translation. Must run after the first window was created (Slint
/// sets up its translation context with the backend).
pub fn select_language(appearance: &Appearance) {
    let language = choose_language(&appearance.languages());
    if let Err(e) = slint::select_bundled_translation(language) {
        tracing::warn!(language, error = ?e, "cannot select translation");
    }
}

/// `Theme.mode` in the UI: 0 = follow the system, 1 = light, 2 = dark.
pub fn theme_mode(appearance: &Appearance) -> i32 {
    match appearance.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn language_choice() {
        assert_eq!(choose_language(&tags(&["zh_TW", "en"])), "zh_TW");
        assert_eq!(choose_language(&tags(&["zh_HK"])), "zh_TW");
        assert_eq!(choose_language(&tags(&["zh"])), "zh_CN");
        assert_eq!(choose_language(&tags(&["fr_FR", "zh_TW"])), "zh_TW");
        assert_eq!(choose_language(&tags(&["en_GB", "zh_TW"])), "en");
        assert_eq!(choose_language(&tags(&["fr_FR"])), "en");
        assert_eq!(choose_language(&[]), "en");
    }

    /// Every `@tr("…")` in ui/*.slint has an entry in every translation.
    #[test]
    fn translations_are_complete() {
        let dir = env!("CARGO_MANIFEST_DIR");
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(format!("{dir}/ui")).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            for part in text.split("@tr(\"").skip(1) {
                // Up to the closing quote, keeping escapes as they are (.po uses the same).
                let mut id = String::new();
                let mut chars = part.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            id.push(c);
                            id.extend(chars.next());
                        }
                        '"' => break,
                        _ => id.push(c),
                    }
                }
                ids.push(id);
            }
        }
        assert!(!ids.is_empty());
        for lang in TRANSLATIONS {
            let po =
                std::fs::read_to_string(format!("{dir}/lang/{lang}/LC_MESSAGES/nibwarden-gui.po"))
                    .unwrap();
            for id in &ids {
                assert!(
                    po.contains(&format!("msgid \"{id}\"")),
                    "{lang} is missing {id:?}"
                );
            }
        }
    }
}
