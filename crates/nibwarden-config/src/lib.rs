//! User settings (phase2.md §5.5): `config.toml` in the platform's config directory.
//! Every key is optional; a missing file means all defaults. Unknown keys are ignored so an
//! older nibwarden can read a newer file.
//!
//! ```toml
//! [appearance]
//! theme = "system"      # "system", "light" or "dark"
//! language = "system"   # "system" or a language tag such as "zh-TW" or "en"
//!
//! [lock]
//! idle_minutes = 15     # lock after this long without vault use; 0 = never
//! on_screen_lock = true # lock the vault when the screen locks
//! on_sleep = true       # lock the vault before suspend or hibernation
//!
//! [approval]                # SSH and git signatures are always confirmed
//! remember = "application"  # what an approval covers: "request", "application" or
//!                           # "terminal" (application + terminal tab)
//! remember_hours = 0        # how long it lasts; 0 = until nibwarden locks
//! read_secret = "ask"       # confirm secrets read by programs: "ask" or "allow"
//! without_window = "allow"  # secret reads when no window can be shown (SSH, no desktop)
//!
//! [clipboard]
//! clear = true          # remove a copied secret from the clipboard after a while
//! clear_after_seconds = 90
//!
//! [tray]
//! enabled = false       # show an icon in the system tray, started at login
//! ```

pub mod integration;

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
    pub lock: Lock,
    pub approval: Approval,
    pub clipboard: ClipboardSettings,
    pub tray: Tray,
}

/// The system tray icon (phase2.md P2-M8). Off by default: GNOME shows it only with the
/// AppIndicator extension.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Tray {
    pub enabled: bool,
}

/// What happens to secrets copied from nibwarden (quick access).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ClipboardSettings {
    pub clear: bool,
    pub clear_after_seconds: u32,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        Self {
            clear: true,
            clear_after_seconds: 90,
        }
    }
}

impl ClipboardSettings {
    /// When to clear a copied secret; `None` = leave it. At least 5 seconds, so a paste
    /// right after copying always works.
    pub fn clear_after(&self) -> Option<std::time::Duration> {
        self.clear
            .then(|| std::time::Duration::from_secs(u64::from(self.clear_after_seconds.max(5))))
    }
}

/// When programs must be confirmed by the user (phase2.md §4.2). SSH signatures (logins,
/// git) are always confirmed; there is no setting for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Approval {
    pub remember: Remember,
    /// How long an approval lasts, like 1Password's "remember key approval"; 0 = until
    /// the vault locks. Every approval also ends when the vault locks.
    pub remember_hours: u32,
    pub read_secret: Ask,
    /// What to do with a secret read that needs confirmation when no window can be shown
    /// for it: from a remote session, or without a desktop. `allow` keeps the CLI usable
    /// over SSH, where unlocking already took the master password.
    pub without_window: Ask,
}

impl Default for Approval {
    fn default() -> Self {
        Self {
            remember: Remember::Application,
            remember_hours: 0,
            read_secret: Ask::Ask,
            without_window: Ask::Allow,
        }
    }
}

/// How far an approval reaches, like 1Password's SSH agent. Every approval ends when the
/// vault locks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Remember {
    /// Only the one request.
    Request,
    /// The application (and everything it starts) until it quits.
    #[default]
    Application,
    /// The application, in one terminal tab.
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ask {
    Ask,
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Lock {
    pub idle_minutes: u32,
    pub on_screen_lock: bool,
    pub on_sleep: bool,
}

impl Default for Lock {
    fn default() -> Self {
        Self {
            idle_minutes: 15,
            on_screen_lock: true,
            on_sleep: true,
        }
    }
}

impl Lock {
    /// `idle_minutes` as a timeout; `None` = never.
    pub fn idle_timeout(&self) -> Option<std::time::Duration> {
        (self.idle_minutes > 0)
            .then(|| std::time::Duration::from_secs(u64::from(self.idle_minutes) * 60))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: Theme,
    /// `system`, or a language tag (`zh-TW`, `zh_TW`, `en`).
    pub language: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            language: "system".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the desktop's light/dark preference.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    /// Languages to try, most preferred first, as POSIX-style tags (`zh_TW`): the
    /// configured one, or the system's.
    pub fn languages(&self) -> Vec<String> {
        if self.language.eq_ignore_ascii_case("system") || self.language.is_empty() {
            return nibwarden_platform::locale::preferred_languages();
        }
        nibwarden_platform::locale::normalize(&self.language)
            .into_iter()
            .collect()
    }
}

/// `config.toml` in the config directory.
pub fn path() -> Option<PathBuf> {
    nibwarden_platform::paths::config_dir()
        .ok()
        .map(|dir| dir.join("config.toml"))
}

/// Loads the settings. Problems never stop the program: a missing file gives the
/// defaults, an unreadable or invalid one is logged and also gives the defaults.
pub fn load() -> Config {
    let Some(path) = path() else {
        return Config::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), error = %e, "ignoring invalid config");
            eprintln!("nibwarden: ignoring invalid {}: {e}", path.display());
            Config::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read config");
            Config::default()
        }
    }
}

/// A value for [`set`].
pub use toml_edit::Value;

/// Changes one setting in `config.toml`, keeping the rest of the file (comments and keys
/// this version does not know) as it is. Creates the file if needed.
pub fn set(section: &str, key: &str, value: impl Into<Value>) -> std::io::Result<()> {
    let path = path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let updated = set_in(&text, section, key, value.into())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Through a temporary file, so a crash never leaves half a config.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, updated)?;
    std::fs::rename(tmp, path)
}

fn set_in(text: &str, section: &str, key: &str, value: Value) -> std::io::Result<String> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let table = doc
        .entry(section)
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("[{section}] is not a table"),
            )
        })?;
    table[key] = toml_edit::value(value);
    Ok(doc.to_string())
}

pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        assert_eq!(parse("").unwrap(), Config::default());
        let config = parse(
            "future = 1\n[appearance]\ntheme = \"dark\"\nlanguage = \"zh-TW\"\nnew_key = true\n",
        )
        .unwrap();
        assert_eq!(config.appearance.theme, Theme::Dark);
        assert_eq!(config.appearance.languages(), ["zh_TW"]);
        assert_eq!(config.lock, Lock::default());
        let config = parse("[lock]\non_sleep = false\n").unwrap();
        assert!(config.lock.on_screen_lock && !config.lock.on_sleep);
        let config = parse(
            "[approval]\nread_secret = \"allow\"\nwithout_window = \"deny\"\nremember = \"terminal\"\nremember_hours = 4\n",
        )
        .unwrap();
        assert_eq!(config.approval.read_secret, Ask::Allow);
        assert_eq!(config.approval.remember, Remember::Terminal);
        assert_eq!(config.approval.remember_hours, 4);
        assert_eq!(config.approval.without_window, Ask::Deny);
    }

    #[test]
    fn set_keeps_the_rest_of_the_file() {
        let text = "# mine\n[appearance]\ntheme = \"dark\" # night owl\n\n[future]\nx = 1\n";
        let updated = set_in(text, "appearance", "language", "zh-TW".into()).unwrap();
        let updated = set_in(&updated, "lock", "idle_minutes", 5i64.into()).unwrap();
        assert!(updated.starts_with("# mine\n"));
        assert!(updated.contains("theme = \"dark\" # night owl"));
        assert!(updated.contains("[future]\nx = 1"));
        let config = parse(&updated).unwrap();
        assert_eq!(config.appearance.languages(), ["zh_TW"]);
        assert_eq!(config.lock.idle_minutes, 5);
        assert!(parse("[appearance]\ntheme = \"blue\"\n").is_err());
    }
}
