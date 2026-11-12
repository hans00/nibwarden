//! An icon in the desktop's system tray with a menu (phase2.md P2-M8). Linux: a
//! StatusNotifierItem, shown by KDE and by GNOME with the AppIndicator extension. macOS
//! (menu bar) and Windows (notification area): made on the thread that runs
//! [`crate::main_thread::wait`].

use std::io;

/// What the icon shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Look {
    /// Closed or open padlock.
    pub locked: bool,
    /// Tooltip: the application's name and a line about its state.
    pub title: String,
    pub detail: String,
    pub menu: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// Text that cannot be chosen.
    Note(String),
    Separator,
    Action {
        label: String,
        enabled: bool,
    },
}

/// The user clicked the icon (`entry` is `None`) or the menu entry at that index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    pub entry: Option<usize>,
    /// Lets a window started for this click come to the front (Wayland's
    /// `XDG_ACTIVATION_TOKEN`), when the desktop gave one.
    pub activation_token: Option<String>,
}

/// The icon, shown until this is dropped.
pub struct Tray(imp::Tray);

impl Tray {
    /// Shows the icon; `chosen` runs on a background thread for each click.
    pub async fn show(
        look: Look,
        chosen: impl Fn(Chosen) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        imp::Tray::show(look, Box::new(chosen)).await.map(Self)
    }

    pub async fn set(&self, look: Look) {
        self.0.set(look).await;
    }
}

type OnChosen = Box<dyn Fn(Chosen) + Send + Sync>;

#[cfg_attr(target_os = "linux", path = "linux/tray.rs")]
#[cfg_attr(any(target_os = "macos", windows), path = "desktop/tray.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/tray.rs"
)]
mod imp;
