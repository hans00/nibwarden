//! An icon in the desktop's system tray with a menu (phase2.md P2-M8). Linux: a
//! StatusNotifierItem, shown by KDE and by GNOME with the AppIndicator extension.

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

#[cfg(target_os = "linux")]
mod imp {
    use std::{io, sync::Arc};

    use ksni::TrayMethods as _;

    use super::{Chosen, Entry, Look, OnChosen};

    struct Item {
        look: Look,
        chosen: Arc<OnChosen>,
        /// nibwarden's own icons are installed (packaging/desktop/icons).
        own_icons: bool,
    }

    impl Item {
        fn choose(&self, entry: Option<usize>) {
            (self.chosen)(Chosen {
                entry,
                activation_token: ksni::current_xdg_activation_token(),
            });
        }
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "nibwarden".into()
        }

        fn title(&self) -> String {
            self.look.title.clone()
        }

        fn icon_name(&self) -> String {
            match (self.own_icons, self.look.locked) {
                (true, true) => "nibwarden-locked-symbolic",
                (true, false) => "nibwarden-unlocked-symbolic",
                // Not installed (a build run in place): the theme's padlocks.
                (false, true) => "changes-prevent-symbolic",
                (false, false) => "changes-allow-symbolic",
            }
            .into()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: self.look.title.clone(),
                description: self.look.detail.clone(),
                ..Default::default()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.choose(None);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            self.look
                .menu
                .iter()
                .enumerate()
                .map(|(index, entry)| match entry {
                    Entry::Separator => ksni::MenuItem::Separator,
                    Entry::Note(label) => ksni::menu::StandardItem {
                        label: label.clone(),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                    Entry::Action { label, enabled } => ksni::menu::StandardItem {
                        label: label.clone(),
                        enabled: *enabled,
                        activate: Box::new(move |item: &mut Self| item.choose(Some(index))),
                        ..Default::default()
                    }
                    .into(),
                })
                .collect()
        }
    }

    pub struct Tray(ksni::Handle<Item>);

    impl Tray {
        pub async fn show(look: Look, chosen: OnChosen) -> io::Result<Self> {
            let item = Item {
                look,
                chosen: Arc::new(chosen),
                own_icons: installed("nibwarden-locked-symbolic"),
            };
            // Started at login, the desktop's tray may not be up yet: wait for it.
            item.assume_sni_available(true)
                .spawn()
                .await
                .map(Self)
                .map_err(io::Error::other)
        }

        pub async fn set(&self, look: Look) {
            self.0.update(move |item| item.look = look).await;
        }
    }

    /// Whether the hicolor theme has the symbolic icon `name`, where the XDG base
    /// directories say icon themes are.
    fn installed(name: &str) -> bool {
        let home = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::home_dir().map(|h| h.join(".local/share")));
        let dirs = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
        home.into_iter()
            .chain(dirs.split(':').map(std::path::PathBuf::from))
            .any(|dir| {
                dir.join("icons/hicolor/symbolic/apps")
                    .join(format!("{name}.svg"))
                    .is_file()
            })
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            // Asks the service to end; no need to wait for it.
            drop(self.0.shutdown());
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::io;

    use super::{Look, OnChosen};

    pub struct Tray;

    impl Tray {
        pub async fn show(_: Look, _: OnChosen) -> io::Result<Self> {
            Err(crate::unsupported())
        }

        pub async fn set(&self, _: Look) {}
    }
}
