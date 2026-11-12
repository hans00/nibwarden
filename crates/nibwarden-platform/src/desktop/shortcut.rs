//! macOS and Windows: the binding is kept in a file in the config directory; the process
//! that holds it (`nibwarden-gui shortcut`) registers it with the `global-hotkey` crate.
//! `command` is not stored: the holder opens quick access itself.

use std::{io, path::PathBuf};

use super::Installed;

pub const NEEDS_HOLDER: bool = true;

fn file() -> io::Result<PathBuf> {
    Ok(crate::paths::config_dir()?.join("quick-access-shortcut"))
}

pub fn supported() -> bool {
    true
}

pub fn current() -> Option<String> {
    let binding = std::fs::read_to_string(file().ok()?).ok()?;
    let binding = binding.trim();
    (!binding.is_empty()).then(|| binding.to_owned())
}

pub fn install(_name: &str, _command: &str, binding: &str) -> io::Result<Installed> {
    // Refuse what the holder could not register.
    hotkey_text(binding)?;
    let file = file()?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&file, format!("{binding}\n"))?;
    Ok(Installed {
        location: file.display().to_string(),
    })
}

pub fn remove() -> io::Result<()> {
    match std::fs::remove_file(file()?) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A GTK accelerator (`<Control><Shift>space`) in `global-hotkey`'s syntax
/// (`control+shift+SPACE`). `<Super>` is Command on macOS, the Windows key on Windows.
fn hotkey_text(binding: &str) -> io::Result<String> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a shortcut: {binding}"),
        )
    };
    let mut parts = Vec::new();
    let mut rest = binding;
    while let Some(after) = rest.strip_prefix('<') {
        let (modifier, tail) = after.split_once('>').ok_or_else(invalid)?;
        parts.push(match modifier {
            "Control" | "Primary" => "control",
            "Alt" => "alt",
            "Shift" => "shift",
            "Super" | "Meta" => "super",
            _ => return Err(invalid()),
        });
        rest = tail;
    }
    let key = match rest {
        "" => return Err(invalid()),
        "Return" => "ENTER".to_owned(),
        "Page_Up" => "PAGEUP".to_owned(),
        "Page_Down" => "PAGEDOWN".to_owned(),
        key => key.to_uppercase(),
    };
    let mut text = parts.join("+");
    if !text.is_empty() {
        text.push('+');
    }
    text.push_str(&key);
    Ok(text)
}

#[cfg(feature = "hotkey")]
pub struct Held {
    manager: global_hotkey::GlobalHotKeyManager,
    hotkey: global_hotkey::hotkey::HotKey,
}

#[cfg(feature = "hotkey")]
impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.manager.unregister(self.hotkey);
    }
}

#[cfg(feature = "hotkey")]
pub fn hold(binding: &str, pressed: Box<dyn Fn() + Send + Sync>) -> io::Result<Held> {
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};

    let hotkey: HotKey = hotkey_text(binding)?
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("{binding}: {e}")))?;
    let manager = GlobalHotKeyManager::new().map_err(io::Error::other)?;
    // Taken by another program (or the system) fails here.
    manager.register(hotkey).map_err(io::Error::other)?;
    let id = hotkey.id();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.id() == id && event.state() == HotKeyState::Pressed {
            pressed();
        }
    }));
    Ok(Held { manager, hotkey })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtk_accelerators_in_global_hotkey_syntax() {
        let text = |b: &str| hotkey_text(b).unwrap();
        assert_eq!(text("<Control><Shift>space"), "control+shift+SPACE");
        assert_eq!(text("<Super>Return"), "super+ENTER");
        assert_eq!(text("<Alt>Page_Down"), "alt+PAGEDOWN");
        assert_eq!(text("F5"), "F5");
        assert_eq!(text("<Control>k"), "control+K");
        assert!(hotkey_text("<Hyper>k").is_err());
        assert!(hotkey_text("<Control>").is_err());
        #[cfg(feature = "hotkey")]
        for binding in [
            "<Control><Shift>space",
            "<Super>Return",
            "<Alt>Page_Down",
            "F5",
        ] {
            text(binding)
                .parse::<global_hotkey::hotkey::HotKey>()
                .unwrap();
        }
    }
}
