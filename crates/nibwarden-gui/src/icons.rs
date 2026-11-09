//! Websites' icons for the vault's items (phase2.md §12.19): asked of the daemon in the
//! background, which gets them from the server's icon service, and kept for the process.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use base64::Engine as _;
use nibwarden_ipc::{Client, IconResult, Request};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

thread_local! {
    static ICONS: RefCell<HashMap<String, Image>> = RefCell::new(HashMap::new());
    /// Domains asked about already, with or without an icon.
    static ASKED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// The domain of the first website among `uris`.
pub fn domain(uris: &[String]) -> Option<String> {
    uris.iter()
        .filter(|u| nibwarden_platform::browser::is_web(u))
        .find_map(|u| host(u))
}

fn host(uri: &str) -> Option<String> {
    let rest = uri.split_once("://").map_or(uri, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split(':').next()?.to_ascii_lowercase();
    let valid = host.contains('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    valid.then_some(host)
}

/// The icon of `domain`, if it is here already.
pub fn get(domain: Option<&str>) -> Image {
    domain
        .and_then(|d| ICONS.with(|icons| icons.borrow().get(d).cloned()))
        .unwrap_or_default()
}

/// Asks for the icons of `domains` not asked about yet, if icons are on; `arrived` runs
/// on the UI thread after each new one.
pub fn request(domains: impl IntoIterator<Item = String>, arrived: fn()) {
    if !nibwarden_config::load().appearance.website_icons {
        return;
    }
    let new: Vec<String> = ASKED.with(|asked| {
        let mut asked = asked.borrow_mut();
        domains
            .into_iter()
            .filter(|d| asked.insert(d.clone()))
            .collect()
    });
    if new.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            let Some(socket) = std::env::var_os("NIBWARDEN_SOCKET")
                .map(PathBuf::from)
                .or_else(nibwarden_ipc::default_socket_path)
            else {
                return;
            };
            let Ok(mut client) = Client::connect(&socket, "nibwarden-gui").await else {
                return;
            };
            for domain in new {
                let Ok(IconResult { image: Some(image) }) = client
                    .call::<IconResult>(Request::ItemIcon {
                        domain: domain.clone(),
                    })
                    .await
                else {
                    continue;
                };
                let Some(pixels) = decode(&image) else {
                    continue;
                };
                let _ = slint::invoke_from_event_loop(move || {
                    ICONS
                        .with(|icons| icons.borrow_mut().insert(domain, Image::from_rgba8(pixels)));
                    arrived();
                });
            }
        });
    });
}

fn decode(base64: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64)
        .ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains() {
        let uris = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            domain(&uris(&[
                "androidapp://x",
                "https://user@GitHub.com:443/login?a"
            ]))
            .as_deref(),
            Some("github.com")
        );
        assert_eq!(
            domain(&uris(&["example.org/path"])).as_deref(),
            Some("example.org")
        );
        assert_eq!(domain(&uris(&["https://localhost/"])), None);
        assert_eq!(domain(&[]), None);
    }
}
