//! Opens an item's website in the user's browser.

use std::io;

/// Opens `uri` (as stored in an item) in the default browser. Only web addresses are
/// opened: an item's URI is data from the vault, and handing `file:` or a custom scheme
/// to the desktop could start other programs. A bare host (`github.com`) gets `https://`.
pub fn open(uri: &str) -> io::Result<()> {
    let url = web_url(uri)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a web address"))?;
    imp::open(&url)
}

/// Whether `open` would open `uri`.
pub fn is_web(uri: &str) -> bool {
    web_url(uri).is_some()
}

fn web_url(uri: &str) -> Option<String> {
    let uri = uri.trim();
    if uri.is_empty() || uri.chars().any(char::is_whitespace) {
        return None;
    }
    match uri.split_once("://") {
        Some((scheme, rest)) => {
            let scheme = scheme.to_ascii_lowercase();
            (matches!(scheme.as_str(), "http" | "https") && !rest.is_empty())
                .then(|| format!("{scheme}://{rest}"))
        }
        // `host:port` or a scheme without `//` (`mailto:`): only the former is a site.
        None if uri.contains(':')
            && !uri.split_once(':').is_some_and(|(_, port)| {
                port.split('/')
                    .next()
                    .is_some_and(|p| p.parse::<u16>().is_ok())
            }) =>
        {
            None
        }
        None if uri.starts_with('-') => None,
        None => Some(format!("https://{uri}")),
    }
}

#[cfg_attr(target_os = "linux", path = "linux/browser.rs")]
#[cfg_attr(target_os = "macos", path = "macos/browser.rs")]
#[cfg_attr(windows, path = "windows/browser.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/browser.rs"
)]
mod imp;

#[cfg(test)]
mod tests {
    use super::web_url;

    #[test]
    fn only_web_addresses() {
        assert_eq!(
            web_url("https://github.com/login").as_deref(),
            Some("https://github.com/login")
        );
        assert_eq!(
            web_url("HTTP://example.test").as_deref(),
            Some("http://example.test")
        );
        assert_eq!(web_url("github.com").as_deref(), Some("https://github.com"));
        assert_eq!(
            web_url("example.test:8443/x").as_deref(),
            Some("https://example.test:8443/x")
        );
        assert_eq!(web_url("file:///etc/passwd"), None);
        assert_eq!(web_url("javascript:alert(1)"), None);
        assert_eq!(web_url("androidapp://com.example"), None);
        assert_eq!(web_url("mailto:a@example.test"), None);
        assert_eq!(web_url("--help"), None);
        assert_eq!(web_url("a b"), None);
    }
}
