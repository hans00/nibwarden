//! macOS and Windows: the `tray-icon` crate. The icon is made and changed on the thread
//! that runs [`crate::main_thread::wait`]; clicks come back through the crate's global
//! event handlers.

use std::{
    cell::RefCell,
    io,
    sync::{Arc, Mutex},
};

use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

use super::{Chosen, Entry, Look, OnChosen};
use crate::main_thread;

/// Menu ids are this followed by the entry's index.
const ID_PREFIX: &str = "nibwarden-";

thread_local! {
    /// The icon, on the main loop's thread (it cannot leave it).
    static ICON: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}

/// Where clicks go; set by [`Tray::show`].
static CHOSEN: Mutex<Option<Arc<OnChosen>>> = Mutex::new(None);

fn choose(entry: Option<usize>) {
    let chosen = CHOSEN.lock().expect("poisoned").clone();
    if let Some(chosen) = chosen {
        chosen(Chosen {
            entry,
            activation_token: None,
        });
    }
}

pub struct Tray;

impl Tray {
    pub async fn show(look: Look, chosen: OnChosen) -> io::Result<Self> {
        *CHOSEN.lock().expect("poisoned") = Some(Arc::new(chosen));
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            if let Some(index) = event
                .id
                .0
                .strip_prefix(ID_PREFIX)
                .and_then(|i| i.parse().ok())
            {
                choose(Some(index));
            }
        }));
        TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
            // Windows: a left click opens the vault, the right button the menu. On macOS
            // any click opens the menu, as status items do there.
            if cfg!(windows)
                && let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
            {
                choose(None);
            }
        }));
        let (made, result) = tokio::sync::oneshot::channel();
        main_thread::post(Box::new(move || {
            let _ = made.send(build(&look));
        }));
        result
            .await
            .map_err(|_| io::Error::other("the main loop ended before the icon was made"))??;
        Ok(Self)
    }

    pub async fn set(&self, look: Look) {
        main_thread::post(Box::new(move || {
            ICON.with(|icon| {
                if let Some(icon) = icon.borrow().as_ref()
                    && let Err(e) = update(icon, &look)
                {
                    tracing::warn!(error = %e, "cannot update the tray icon");
                }
            });
        }));
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // Dropping the icon removes it (otherwise Windows keeps a dead one until hovered).
        main_thread::post(Box::new(|| {
            drop(ICON.with(|icon| icon.borrow_mut().take()))
        }));
    }
}

fn build(look: &Look) -> io::Result<()> {
    let builder = TrayIconBuilder::new()
        .with_id("nibwarden")
        .with_tooltip(tooltip(look))
        .with_menu(Box::new(menu(look)?))
        .with_menu_on_left_click(!cfg!(windows));
    // macOS draws template images in the menu bar's own colour.
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon(look.locked)?);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(icon(look.locked)?);
    let icon = builder.build().map_err(io::Error::other)?;
    ICON.with(|slot| *slot.borrow_mut() = Some(icon));
    Ok(())
}

fn update(icon: &TrayIcon, look: &Look) -> io::Result<()> {
    let image = Some(self::icon(look.locked)?);
    #[cfg(target_os = "macos")]
    icon.set_icon_templated(image).map_err(io::Error::other)?;
    #[cfg(not(target_os = "macos"))]
    icon.set_icon(image).map_err(io::Error::other)?;
    icon.set_tooltip(Some(tooltip(look)))
        .map_err(io::Error::other)?;
    icon.set_menu(Some(Box::new(menu(look)?)));
    Ok(())
}

fn tooltip(look: &Look) -> String {
    if look.detail.is_empty() {
        look.title.clone()
    } else {
        format!("{}\n{}", look.title, look.detail)
    }
}

fn menu(look: &Look) -> io::Result<Menu> {
    let menu = Menu::new();
    for (index, entry) in look.menu.iter().enumerate() {
        let result = match entry {
            Entry::Separator => menu.append(&PredefinedMenuItem::separator()),
            Entry::Note(label) => menu.append(&MenuItem::new(label, false, None)),
            Entry::Action { label, enabled } => menu.append(&MenuItem::with_id(
                format!("{ID_PREFIX}{index}"),
                label,
                *enabled,
                None,
            )),
        };
        result.map_err(io::Error::other)?;
    }
    Ok(menu)
}

/// The padlocks from packaging/desktop/icons, as 32×32 RGBA (assets/tray/README.md).
fn icon(locked: bool) -> io::Result<Icon> {
    let rgba: &[u8] = match (locked, dark_background()) {
        (true, false) => include_bytes!("../../assets/tray/locked-black.rgba"),
        (true, true) => include_bytes!("../../assets/tray/locked-white.rgba"),
        (false, false) => include_bytes!("../../assets/tray/unlocked-black.rgba"),
        (false, true) => include_bytes!("../../assets/tray/unlocked-white.rgba"),
    };
    Icon::from_rgba(rgba.to_vec(), 32, 32).map_err(io::Error::other)
}

/// macOS recolours the template image itself; black is the template's colour.
#[cfg(target_os = "macos")]
fn dark_background() -> bool {
    false
}

/// Windows: whether the taskbar is dark ("Choose your default Windows mode"); it is
/// unless `SystemUsesLightTheme` says otherwise.
#[cfg(windows)]
fn dark_background() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut light: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated and `light` has room for the DWORD asked for.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut light).cast(),
            &mut size,
        )
    };
    status != 0 || light == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_and_menu_ids() {
        for locked in [true, false] {
            icon(locked).unwrap();
        }
        assert_eq!(format!("{ID_PREFIX}3").strip_prefix(ID_PREFIX), Some("3"));
    }
}
