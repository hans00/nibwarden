//! Website icons, from the icon service of the account's server, as Bitwarden's clients
//! show them: `icons.bitwarden.net` for bitwarden.com, `icons.bitwarden.eu` for
//! bitwarden.eu, `<server>/icons` for a self-hosted one (Vaultwarden has the same).
//!
//! Requests go through a client that is not logged in, so the access token never reaches
//! the icon service; it learns the domain asked about, which is why the GUI has a setting
//! to turn icons off.

use std::sync::OnceLock;

use bitwarden_core::{ClientBuilder, get_host_platform_info};

use crate::{Error, error::SdkContext as _};

/// The most an icon may be; larger answers are not icons.
const MAX_ICON: usize = 256 * 1024;

/// The icon service for the server whose API is at `api_url`.
pub fn icons_url(api_url: &str) -> String {
    let api = api_url.trim_end_matches('/');
    match api {
        "https://api.bitwarden.com" => "https://icons.bitwarden.net".to_owned(),
        "https://api.bitwarden.eu" => "https://icons.bitwarden.eu".to_owned(),
        _ => format!("{}/icons", api.strip_suffix("/api").unwrap_or(api)),
    }
}

/// Whether `domain` is a host name the icon service can be asked about (and nothing that
/// would change the request's path).
pub fn valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= 253
        && domain.contains('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !domain.starts_with(['.', '-'])
}

/// The icon of `domain` from the icon service at `base`: its bytes as served (PNG
/// usually, an ICO or JPEG at times); `None` when the service has none.
pub async fn fetch_icon(base: &str, domain: &str) -> Result<Option<Vec<u8>>, Error> {
    if !valid_domain(domain) {
        return Err(Error::Invalid(format!("not a domain: {domain}")));
    }
    // One client for every icon, built without an account.
    static CLIENT: OnceLock<bitwarden_core::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        let settings = get_host_platform_info().to_client_settings(String::new(), String::new());
        ClientBuilder::new().with_settings(settings).build()
    });
    let http = &client
        .internal
        .get_api_configurations()
        .identity_config
        .client;
    let response = http
        .get(format!("{base}/{domain}/icon.png"))
        .send()
        .await
        .sdk("reach the icon service")?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let bytes = response.bytes().await.sdk("read the icon")?;
    Ok((!bytes.is_empty() && bytes.len() <= MAX_ICON).then(|| bytes.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_services() {
        assert_eq!(
            icons_url("https://api.bitwarden.com"),
            "https://icons.bitwarden.net"
        );
        assert_eq!(
            icons_url("https://api.bitwarden.eu/"),
            "https://icons.bitwarden.eu"
        );
        assert_eq!(
            icons_url("https://vault.example.test/api"),
            "https://vault.example.test/icons"
        );
    }

    #[test]
    fn domains() {
        assert!(valid_domain("github.com"));
        assert!(valid_domain("my-host.example.co.uk"));
        for bad in [
            "",
            "localhost",
            "a.com/../x",
            "a.com?x",
            ".a.com",
            "a b.com",
        ] {
            assert!(!valid_domain(bad), "{bad}");
        }
    }
}
