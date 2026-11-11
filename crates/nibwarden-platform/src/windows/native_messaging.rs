//! Native messaging hosts on Windows: a registry key per browser whose default value is the
//! path of the manifest file (`HKCU\Software\<vendor>\<browser>\NativeMessagingHosts\<host>`),
//! and the manifest files, which are kept under nibwarden's data directory.

use std::{
    io,
    path::{Path, PathBuf},
};

use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData};

use super::{BITWARDEN_HOST, Browser, Registered, manifest, registered};
use crate::winutil::{known_folder, registry};

/// One browser: where it keeps its profile (which tells that it is installed), and which
/// registry key it reads hosts from.
struct Spec {
    name: &'static str,
    /// Under `HKCU\<root>`: the browser's own key.
    key: &'static str,
    /// Directory name of the manifest, under nibwarden's `native-messaging`.
    id: &'static str,
    profile: PathBuf,
    firefox: bool,
}

/// Where everything is; the real one, or one pointed at scratch places by tests.
struct Layout {
    /// Registry path the vendor keys are under: `Software`.
    root: String,
    /// Where manifests are written.
    manifests: PathBuf,
    browsers: Vec<Spec>,
}

impl Layout {
    fn real() -> io::Result<Self> {
        let local = known_folder(&FOLDERID_LocalAppData)?;
        let roaming = known_folder(&FOLDERID_RoamingAppData)?;
        let spec = |name, key, id, profile: PathBuf, firefox| Spec {
            name,
            key,
            id,
            profile,
            firefox,
        };
        Ok(Self {
            root: "Software".to_owned(),
            manifests: crate::paths::data_dir()?.join("native-messaging"),
            browsers: vec![
                spec(
                    "Firefox",
                    r"Mozilla",
                    "firefox",
                    roaming.join(r"Mozilla\Firefox"),
                    true,
                ),
                spec(
                    "Google Chrome",
                    r"Google\Chrome",
                    "chrome",
                    local.join(r"Google\Chrome\User Data"),
                    false,
                ),
                spec(
                    "Chromium",
                    r"Chromium",
                    "chromium",
                    local.join(r"Chromium\User Data"),
                    false,
                ),
                spec(
                    "Microsoft Edge",
                    r"Microsoft\Edge",
                    "edge",
                    local.join(r"Microsoft\Edge\User Data"),
                    false,
                ),
                spec(
                    "Vivaldi",
                    r"Vivaldi",
                    "vivaldi",
                    local.join(r"Vivaldi\User Data"),
                    false,
                ),
                spec(
                    "Brave",
                    r"BraveSoftware\Brave-Browser",
                    "brave",
                    local.join(r"BraveSoftware\Brave-Browser\User Data"),
                    false,
                ),
            ],
        })
    }

    fn registry_key(&self, spec: &Spec) -> String {
        format!(
            r"{}\{}\NativeMessagingHosts\{BITWARDEN_HOST}",
            self.root, spec.key
        )
    }

    fn manifest_path(&self, spec: &Spec) -> PathBuf {
        self.manifests
            .join(spec.id)
            .join(format!("{BITWARDEN_HOST}.json"))
    }

    fn installed(&self) -> impl Iterator<Item = &Spec> {
        self.browsers.iter().filter(|spec| spec.profile.is_dir())
    }

    /// Who the browser starts, by the manifest its key names.
    fn registered(&self, spec: &Spec, host: &Path) -> Registered {
        match registry::get_string(&self.registry_key(spec), None) {
            Ok(Some(path)) => registered(Path::new(&path), host),
            _ => Registered::Nothing,
        }
    }
}

fn write_manifest(layout: &Layout, spec: &Spec, host: &Path) -> io::Result<()> {
    let path = layout.manifest_path(spec);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text =
        serde_json::to_string_pretty(&manifest(host, spec.firefox)).expect("a manifest serializes");
    std::fs::write(&path, text + "\n")?;
    registry::set_string(
        &layout.registry_key(spec),
        None,
        &path.display().to_string(),
    )
}

fn browsers_in(layout: &Layout, host: &Path) -> Vec<Browser> {
    layout
        .installed()
        .map(|spec| Browser {
            name: spec.name,
            registered: layout.registered(spec, host),
        })
        .collect()
}

fn register_in(layout: &Layout, host: &Path, replace: bool) -> io::Result<Vec<Browser>> {
    for spec in layout.installed() {
        if matches!(layout.registered(spec, host), Registered::Other(_)) && !replace {
            continue;
        }
        write_manifest(layout, spec, host)?;
    }
    Ok(browsers_in(layout, host))
}

fn refresh_in(layout: &Layout, host: &Path) -> io::Result<()> {
    for spec in layout.installed() {
        if layout.registered(spec, host) != Registered::Nibwarden {
            continue;
        }
        let ours = layout.manifest_path(spec);
        let current = std::fs::read_to_string(&ours).unwrap_or_default();
        let wanted = manifest(host, spec.firefox);
        let same = serde_json::from_str::<serde_json::Value>(&current).is_ok_and(|m| m == wanted);
        // The key may name a manifest of nibwarden elsewhere, from an older installation.
        let key_here = registry::get_string(&layout.registry_key(spec), None)
            .ok()
            .flatten()
            .is_some_and(|p| Path::new(&p) == ours);
        if !same || !key_here {
            write_manifest(layout, spec, host)?;
        }
    }
    Ok(())
}

fn unregister_in(layout: &Layout, host: &Path) -> io::Result<()> {
    // Every known browser, installed or not: the key is what counts.
    for spec in &layout.browsers {
        if layout.registered(spec, host) == Registered::Nibwarden {
            registry::delete_key(&layout.registry_key(spec))?;
            match std::fs::remove_file(layout.manifest_path(spec)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn browsers(host: &Path) -> Vec<Browser> {
    Layout::real().map_or_else(|_| Vec::new(), |layout| browsers_in(&layout, host))
}

pub fn register(host: &Path, replace: bool) -> io::Result<Vec<Browser>> {
    register_in(&Layout::real()?, host, replace)
}

pub fn refresh(host: &Path) -> io::Result<()> {
    refresh_in(&Layout::real()?, host)
}

pub fn unregister(host: &Path) -> io::Result<()> {
    unregister_in(&Layout::real()?, host)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layout in a scratch registry key and scratch directories, with Chrome and Firefox
    /// "installed". Never touches the real browsers' keys.
    fn scratch(name: &str) -> (Layout, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nw-nmh-win-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spec = |name, key, id, firefox, installed: bool| {
            let profile = dir.join("profiles").join(id);
            if installed {
                std::fs::create_dir_all(&profile).unwrap();
            }
            Spec {
                name,
                key,
                id,
                profile,
                firefox,
            }
        };
        let layout = Layout {
            root: format!(r"Software\NibwardenTest-{}-{name}", std::process::id()),
            manifests: dir.join("manifests"),
            browsers: vec![
                spec("Firefox", "Mozilla", "firefox", true, true),
                spec("Google Chrome", r"Google\Chrome", "chrome", false, true),
                spec("Chromium", "Chromium", "chromium", false, false),
            ],
        };
        (layout, dir)
    }

    fn cleanup(layout: &Layout, dir: &Path) {
        for spec in &layout.browsers {
            let _ = registry::delete_key(&layout.registry_key(spec));
            let _ = registry::delete_key(&format!(
                r"{}\{}\NativeMessagingHosts",
                layout.root, spec.key
            ));
            let _ = registry::delete_key(&format!(r"{}\{}", layout.root, spec.key));
        }
        let _ = registry::delete_key(&layout.root);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn registers_refreshes_and_unregisters_in_installed_browsers() {
        let (layout, dir) = scratch("cycle");
        let host = Path::new(r"C:\Program Files\nibwarden\nibwarden-browser-host.exe");
        let state = |l: &Layout| {
            browsers_in(l, host)
                .into_iter()
                .map(|b| (b.name, b.registered))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            state(&layout),
            [
                ("Firefox", Registered::Nothing),
                ("Google Chrome", Registered::Nothing)
            ]
        );

        register_in(&layout, host, false).unwrap();
        assert_eq!(
            state(&layout),
            [
                ("Firefox", Registered::Nibwarden),
                ("Google Chrome", Registered::Nibwarden)
            ]
        );
        // The key names the manifest, which names the host; Chromium is not installed.
        let chrome = &layout.browsers[1];
        let key = registry::get_string(&layout.registry_key(chrome), None)
            .unwrap()
            .unwrap();
        assert_eq!(Path::new(&key), layout.manifest_path(chrome));
        let text = std::fs::read_to_string(&key).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["path"], host.to_str().unwrap());
        assert!(json.get("allowed_origins").is_some());
        assert!(
            registry::get_string(&layout.registry_key(&layout.browsers[2]), None)
                .unwrap()
                .is_none()
        );

        // Another program in Chrome stays unless replaced.
        let other = dir.join("bitwarden.json");
        std::fs::write(&other, r#"{"path": "C:\\Bitwarden\\desktop_proxy.exe"}"#).unwrap();
        registry::set_string(
            &layout.registry_key(chrome),
            None,
            &other.display().to_string(),
        )
        .unwrap();
        register_in(&layout, host, false).unwrap();
        assert_eq!(
            layout.registered(chrome, host),
            Registered::Other(r"C:\Bitwarden\desktop_proxy.exe".into())
        );
        register_in(&layout, host, true).unwrap();
        assert_eq!(layout.registered(chrome, host), Registered::Nibwarden);

        // A moved installation is followed.
        let moved = Path::new(r"D:\apps\nibwarden\nibwarden-browser-host.exe");
        refresh_in(&layout, moved).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(layout.manifest_path(chrome)).unwrap())
                .unwrap();
        assert_eq!(json["path"], moved.to_str().unwrap());

        unregister_in(&layout, moved).unwrap();
        assert_eq!(
            state(&layout),
            [
                ("Firefox", Registered::Nothing),
                ("Google Chrome", Registered::Nothing)
            ]
        );
        assert!(!layout.manifest_path(chrome).exists());
        cleanup(&layout, &dir);
    }
}
