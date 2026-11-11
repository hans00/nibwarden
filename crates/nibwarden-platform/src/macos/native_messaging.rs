use std::{
    io,
    path::{Path, PathBuf},
};

use super::{BITWARDEN_HOST, Browser, Registered, manifest, registered};

/// A browser's settings directory under `~/Library/Application Support` (where it is
/// installed, if present), the directory it reads host manifests from (`NativeMessagingHosts`
/// below it, except Firefox's, which lives under `Mozilla`), and whether it is Firefox
/// (extension ids rather than origins in the manifest). Each path is the one the
/// browser's documentation names.
const BROWSERS: [(&str, &str, &str, bool); 6] = [
    ("Firefox", "Firefox", "Mozilla", true),
    ("Google Chrome", "Google/Chrome", "Google/Chrome", false),
    ("Chromium", "Chromium", "Chromium", false),
    ("Microsoft Edge", "Microsoft Edge", "Microsoft Edge", false),
    ("Vivaldi", "Vivaldi", "Vivaldi", false),
    (
        "Brave",
        "BraveSoftware/Brave-Browser",
        "BraveSoftware/Brave-Browser",
        false,
    ),
];

fn support_dir() -> io::Result<PathBuf> {
    crate::paths::data_dir()?
        .parent()
        .map(Path::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no Application Support directory"))
}

/// The installed browsers: name, manifest path, Firefox or not.
fn installed() -> io::Result<Vec<(&'static str, PathBuf, bool)>> {
    let support = support_dir()?;
    Ok(BROWSERS
        .iter()
        .filter(|(_, present, _, _)| support.join(present).is_dir())
        .map(|&(name, _, hosts, firefox)| {
            let path = support
                .join(hosts)
                .join("NativeMessagingHosts")
                .join(format!("{BITWARDEN_HOST}.json"));
            (name, path, firefox)
        })
        .collect())
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
