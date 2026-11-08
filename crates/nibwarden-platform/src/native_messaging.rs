//! Native messaging host manifests (phase2.md §5.8): the files that tell a browser which
//! program to start when an extension connects to a host name. nibwarden registers
//! `nibwarden-browser-host` under the name of Bitwarden's desktop app, for the official
//! Bitwarden extension.
// Only Linux registers hosts so far; the manifest helpers wait for the other platforms.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::{io, path::Path};

/// The host name the Bitwarden extension connects to.
pub const BITWARDEN_HOST: &str = "com.8bit.bitwarden";

/// The official Bitwarden extensions allowed to start the host (Chrome Web Store, its beta,
/// Edge and Opera add-ons), as Bitwarden's desktop app lists them.
const CHROME_ORIGINS: [&str; 4] = [
    "chrome-extension://nngceckbapebfimnlniiiahkandclblb/",
    "chrome-extension://hccnnhgbibccigepcmlgppchkpfdophk/",
    "chrome-extension://jbkfoedolllekgbhcbcoahefnbanhhlh/",
    "chrome-extension://ccnckbpmaceehanjmeomladnmlffdjgn/",
];
const FIREFOX_EXTENSIONS: [&str; 1] = ["{446900e4-71c2-419f-a6a7-df9c091e268b}"];

/// Who a browser starts for the Bitwarden extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registered {
    /// `nibwarden-browser-host`.
    Nibwarden,
    /// Another program, e.g. Bitwarden's own desktop app.
    Other(String),
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    pub name: &'static str,
    pub registered: Registered,
}

/// The installed browsers (those with a settings directory) and what each starts.
pub fn browsers(host: &Path) -> Vec<Browser> {
    imp::browsers(host)
}

/// Registers `host` (the `nibwarden-browser-host` program) in every installed browser.
/// A browser where another program is registered is left alone unless `replace`.
/// Returns the browsers afterwards.
pub fn register(host: &Path, replace: bool) -> io::Result<Vec<Browser>> {
    imp::register(host, replace)
}

/// Points nibwarden's registrations at `host`, where they name another copy of it (e.g.
/// an older Nix store path); registers nowhere new.
pub fn refresh(host: &Path) -> io::Result<()> {
    imp::refresh(host)
}

/// Removes nibwarden's registration from every browser; others' stay.
pub fn unregister(host: &Path) -> io::Result<()> {
    imp::unregister(host)
}

/// The manifest for a Chromium-based browser or for Firefox.
fn manifest(host: &Path, firefox: bool) -> serde_json::Value {
    let mut manifest = serde_json::json!({
        "name": BITWARDEN_HOST,
        "description": "nibwarden <-> Bitwarden browser extension",
        "path": host,
        "type": "stdio",
    });
    if firefox {
        manifest["allowed_extensions"] = serde_json::json!(FIREFOX_EXTENSIONS);
    } else {
        manifest["allowed_origins"] = serde_json::json!(CHROME_ORIGINS);
    }
    manifest
}

/// Whether the manifest at `path` starts nibwarden's host (by program name, so a moved
/// installation still counts as nibwarden's).
fn registered(path: &Path, host: &Path) -> Registered {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Registered::Nothing;
    };
    let program = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|m| m.get("path")?.as_str().map(str::to_owned))
        .unwrap_or_default();
    if Path::new(&program).file_name().is_some()
        && Path::new(&program).file_name() == host.file_name()
    {
        Registered::Nibwarden
    } else {
        Registered::Other(program)
    }
}

#[cfg(target_os = "linux")]
mod imp {
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
            let text = serde_json::to_string_pretty(&manifest(host, firefox))
                .expect("a manifest serializes");
            std::fs::write(&path, text + "\n")?;
        }
        Ok(browsers(host))
    }

    pub fn refresh(host: &Path) -> io::Result<()> {
        for (_, path, firefox) in installed()? {
            let current = std::fs::read_to_string(&path).unwrap_or_default();
            let wanted = manifest(host, firefox);
            let same =
                serde_json::from_str::<serde_json::Value>(&current).is_ok_and(|m| m == wanted);
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
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::{io, path::Path};

    use super::Browser;

    pub fn browsers(_: &Path) -> Vec<Browser> {
        Vec::new()
    }

    pub fn register(_: &Path, _: bool) -> io::Result<Vec<Browser>> {
        Err(crate::unsupported())
    }

    pub fn refresh(_: &Path) -> io::Result<()> {
        Ok(())
    }

    pub fn unregister(_: &Path) -> io::Result<()> {
        Err(crate::unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_recognises_manifests() {
        let dir = std::env::temp_dir().join(format!("nw-nmh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = Path::new("/usr/bin/nibwarden-browser-host");
        let chrome = manifest(host, false);
        assert_eq!(chrome["allowed_origins"][0], CHROME_ORIGINS[0]);
        assert!(chrome.get("allowed_extensions").is_none());
        let firefox = manifest(host, true);
        assert_eq!(firefox["allowed_extensions"][0], FIREFOX_EXTENSIONS[0]);

        let path = dir.join("m.json");
        assert_eq!(registered(&path, host), Registered::Nothing);
        std::fs::write(&path, firefox.to_string()).unwrap();
        // Recognised by program name, wherever it is installed.
        assert_eq!(
            registered(&path, Path::new("/opt/nw/nibwarden-browser-host")),
            Registered::Nibwarden
        );
        std::fs::write(&path, r#"{"path": "/opt/Bitwarden/desktop_proxy"}"#).unwrap();
        assert_eq!(
            registered(&path, host),
            Registered::Other("/opt/Bitwarden/desktop_proxy".into())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
