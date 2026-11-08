/// API and identity endpoints of a Bitwarden-compatible server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub api_url: String,
    pub identity_url: String,
}

impl Server {
    /// Accepts `bitwarden.com` / `us`, `bitwarden.eu` / `eu`, or the base URL of a
    /// self-hosted server (Vaultwarden or Bitwarden self-host), e.g. `https://vault.example.com`.
    pub fn parse(input: &str) -> Self {
        match input.trim() {
            "bitwarden.com" | "us" => Self::cloud("bitwarden.com"),
            "bitwarden.eu" | "eu" => Self::cloud("bitwarden.eu"),
            base => {
                let base = base.trim_end_matches('/');
                Self {
                    api_url: format!("{base}/api"),
                    identity_url: format!("{base}/identity"),
                }
            }
        }
    }

    /// Both endpoints use TLS. Release builds of the SDK refuse anything else.
    pub fn is_https(&self) -> bool {
        self.api_url.starts_with("https://") && self.identity_url.starts_with("https://")
    }

    fn cloud(domain: &str) -> Self {
        Self {
            api_url: format!("https://api.{domain}"),
            identity_url: format!("https://identity.{domain}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_aliases() {
        assert_eq!(Server::parse("us"), Server::parse("bitwarden.com"));
        assert_eq!(Server::parse("eu").api_url, "https://api.bitwarden.eu");
    }

    #[test]
    fn self_hosted_strips_trailing_slash() {
        let s = Server::parse("http://127.0.0.1:8222/");
        assert_eq!(s.api_url, "http://127.0.0.1:8222/api");
        assert_eq!(s.identity_url, "http://127.0.0.1:8222/identity");
        assert!(!s.is_https());
        assert!(Server::parse("https://vault.example.test").is_https());
        assert!(Server::parse("eu").is_https());
    }
}
