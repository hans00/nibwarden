use std::{
    io,
    path::{Path, PathBuf},
};

use super::{BITWARDEN_HOST, Browser, Registered, manifest, registered};

/// Each browser's settings directory under the home directory, and whether it is
/// Firefox (`native-messaging-hosts` and extension ids rather than
/// `NativeMessagingHosts` and origins). Bitwarden's desktop app covers the same ones.
const BROWSERS: [(&str, &str, bool); 7] = [
    ("Firefox", ".mozilla", true),
    ("Google Chrome", ".config/google-chrome", false),
    ("Chromium", ".config/chromium", false),
    ("Microsoft Edge", ".config/microsoft-edge", false),
    ("Vivaldi", ".config/vivaldi", false),
    ("Brave", ".config/BraveSoftware/Brave-Browser", false),
    ("Helium", ".config/net.imput.helium", false),
];

fn home() -> io::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}

/// The installed browsers: name, manifest path, Firefox or not.
fn installed() -> io::Result<Vec<(&'static str, PathBuf, bool)>> {
    let home = home()?;
    let mut found = Vec::new();
    for (name, dir, firefox) in BROWSERS {
        let mut dir = home.join(dir);
        // Firefox 147 and later may keep profiles under ~/.config/mozilla only; it
        // still reads hosts from ~/.mozilla.
        let present = dir.is_dir() || (firefox && home.join(".config/mozilla").is_dir());
        if !present {
            continue;
        }
        dir.push(if firefox {
            "native-messaging-hosts"
        } else {
            "NativeMessagingHosts"
        });
        found.push((name, dir.join(format!("{BITWARDEN_HOST}.json")), firefox));
    }
    Ok(found)
}

pub fn browsers(host: &Path) -> Vec<Browser> {
    installed()
        .unwrap_or_default()
        .into_iter()
        .map(|(name, path, _)| Browser {
            name,
            registered: registered(&path, host),
        })
        .collect()
}

pub fn register(host: &Path, replace: bool) -> io::Result<Vec<Browser>> {
    for (_, path, firefox) in installed()? {
        if matches!(registered(&path, host), Registered::Other(_)) && !replace {
            continue;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text =
            serde_json::to_string_pretty(&manifest(host, firefox)).expect("a manifest serializes");
        std::fs::write(&path, text + "\n")?;
    }
    Ok(browsers(host))
}

pub fn refresh(host: &Path) -> io::Result<()> {
    for (_, path, firefox) in installed()? {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        let wanted = manifest(host, firefox);
        let same = serde_json::from_str::<serde_json::Value>(&current).is_ok_and(|m| m == wanted);
        if registered(&path, host) == Registered::Nibwarden && !same {
            let text = serde_json::to_string_pretty(&wanted).expect("a manifest serializes");
            std::fs::write(&path, text + "\n")?;
        }
    }
    Ok(())
}

pub fn unregister(host: &Path) -> io::Result<()> {
    for (_, path, _) in installed()? {
        if registered(&path, host) == Registered::Nibwarden {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}
