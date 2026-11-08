//! Sends (phase2.md P2-M12): text or a file shared through a link, encrypted with a key
//! that only the link carries. The SDK keeps them in its state database (synced by its own
//! sync handler) and does the encryption; this adds the share link and the checks.

use bitwarden_send::{
    SendAddRequest, SendAuthType, SendFileView, SendId, SendTextView, SendType, SendView,
    SendViewType,
};
use zeroize::Zeroizing;

use crate::{Account, Error, error::SdkContext as _};

/// Largest file a Send can carry: the file travels to the daemon in one control message.
pub const MAX_SEND_FILE: usize = 10 * 1024 * 1024;

/// Days a new Send can live before the server deletes it (Bitwarden allows at most 31).
pub const MAX_SEND_DAYS: u32 = 31;

/// A Send as listed: what it is and when it goes away, not its content or link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendInfo {
    pub id: String,
    pub name: String,
    /// `None` for a text Send.
    pub file_name: Option<String>,
    /// The file's size as the server describes it ("1.2 MB").
    pub file_size: Option<String>,
    /// RFC 3339.
    pub deletion_date: String,
    pub expiration_date: Option<String>,
    pub access_count: u32,
    pub max_access_count: Option<u32>,
    pub disabled: bool,
    pub has_password: bool,
    pub hide_email: bool,
    pub notes: Option<String>,
}

/// What lets someone read a Send: the link (which carries the key), and its text.
#[derive(Debug, Clone)]
pub struct SendSecret {
    pub link: Zeroizing<String>,
    pub text: Option<Zeroizing<String>>,
    /// The text is hidden behind a button on the receiving page.
    pub hidden: bool,
}

/// What a new Send carries.
pub enum SendBody {
    Text {
        text: Zeroizing<String>,
        hidden: bool,
    },
    File {
        name: String,
        data: Zeroizing<Vec<u8>>,
    },
}

/// A new Send.
pub struct NewSend {
    pub name: String,
    pub notes: Option<String>,
    pub body: SendBody,
    /// Deleted by the server after this many days, 1 to [`MAX_SEND_DAYS`].
    pub deletion_days: u32,
    /// Stops opening after this many accesses.
    pub max_access_count: Option<u32>,
    pub password: Option<Zeroizing<String>>,
    pub hide_email: bool,
}

impl Account {
    pub async fn list_sends(&self) -> Result<Vec<SendInfo>, Error> {
        if !self.is_unlocked() {
            return Err(Error::Locked);
        }
        let mut sends: Vec<SendInfo> = self
            .client
            .sends()
            .list()
            .await
            .sdk("list sends")?
            .iter()
            .filter_map(info)
            .collect();
        sends.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        Ok(sends)
    }

    pub async fn send_info(&self, id: &str) -> Result<SendInfo, Error> {
        info(&self.send_view(id).await?).ok_or_else(|| not_found(id))
    }

    /// The link and text of a Send.
    pub async fn send_secret(&self, id: &str) -> Result<SendSecret, Error> {
        let view = self.send_view(id).await?;
        let link = link(&self.web_vault_url().await?, &view)?;
        let (text, hidden) = match view.text {
            Some(SendTextView { text, hidden }) => (text.map(Zeroizing::new), hidden),
            None => (None, false),
        };
        Ok(SendSecret { link, text, hidden })
    }

    /// Creates a Send and returns its id.
    pub async fn create_send(&self, send: NewSend) -> Result<String, Error> {
        if !self.is_unlocked() {
            return Err(Error::Locked);
        }
        let request = add_request(send.name, send.notes, &send.body, send.deletion_days)?;
        let request = SendAddRequest {
            max_access_count: send.max_access_count.filter(|n| *n > 0),
            hide_email: send.hide_email,
            auth: match send.password.as_deref().map(|p| p.as_str()) {
                Some(password) if !password.is_empty() => SendAuthType::Password {
                    password: password.to_owned(),
                },
                _ => SendAuthType::None,
            },
            ..request
        };
        let sends = self.client.sends();
        let view = match send.body {
            SendBody::Text { .. } => sends.create(request).await.sdk("create send")?,
            SendBody::File { data, .. } => {
                let created = sends
                    .create_file_send(request, data.to_vec())
                    .await
                    .sdk("create file send")?;
                let id = created.send.id.ok_or_else(|| {
                    Error::State("the server returned a Send without an id".into())
                })?;
                let uploaded = sends
                    .upload_send_file(
                        id,
                        created.file_id,
                        created.encrypted_file_name,
                        created.file_upload_type,
                        created.url,
                        created.encrypted_file_buffer,
                    )
                    .await
                    .sdk("upload send file");
                if let Err(e) = uploaded {
                    // A Send without its file is useless: do not leave it behind.
                    let _ = sends.delete(id).await;
                    return Err(e);
                }
                created.send
            }
        };
        view.id
            .map(|id| id.to_string())
            .ok_or_else(|| Error::State("the server returned a Send without an id".into()))
    }

    pub async fn delete_send(&self, id: &str) -> Result<(), Error> {
        let view = self.send_view(id).await?;
        let id = view.id.ok_or_else(|| not_found(""))?;
        self.client.sends().delete(id).await.sdk("delete send")
    }

    async fn send_view(&self, id: &str) -> Result<SendView, Error> {
        if !self.is_unlocked() {
            return Err(Error::Locked);
        }
        let send_id: SendId = id.parse().map_err(|_| not_found(id))?;
        self.client
            .sends()
            .get(send_id)
            .await
            .map_err(|_| not_found(id))
    }

    /// Where the server's web vault is, which also opens Send links.
    async fn web_vault_url(&self) -> Result<String, Error> {
        Ok(web_vault(&self.api_url().await?))
    }
}

/// What a recipient sees after opening a link (for tests: the browser does this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedSend {
    pub name: Option<String>,
    pub text: Option<String>,
    pub file_name: Option<String>,
    /// Where to download the encrypted file ([`decrypt_send_file`] opens it).
    pub file_url: Option<String>,
}

/// Splits a Send link into the access id and the key.
pub fn parse_send_link(link: &str) -> Option<(&str, &str)> {
    let rest = link.split_once("#/send/")?.1;
    let (access_id, key) = rest.split_once('/')?;
    Some((access_id, key.trim_end_matches('/')))
}

/// The server a link belongs to: bitwarden.com's web vault is at `vault.<domain>`, a
/// self-hosted one shares its base URL with the API.
fn server_of_link(link: &str) -> Option<crate::Server> {
    let base = link.split_once("/#/send/")?.0.trim_end_matches('/');
    Some(match base.strip_prefix("https://vault.") {
        Some(domain @ ("bitwarden.com" | "bitwarden.eu")) => crate::Server::parse(domain),
        _ => crate::Server::parse(base),
    })
}

/// Opens a Send link as a recipient: an access token for the Send from the identity
/// service, then its content (Vaultwarden 1.37 and bitwarden.com no longer accept the
/// older access requests without a token).
pub async fn receive_send(link: &str) -> Result<ReceivedSend, Error> {
    let invalid = || Error::Invalid("not a Send link".into());
    let (access_id, key) = parse_send_link(link).ok_or_else(invalid)?;
    let server = server_of_link(link).ok_or_else(invalid)?;
    let settings = bitwarden_core::get_host_platform_info()
        .to_client_settings(server.api_url, server.identity_url);
    let receive = bitwarden_send::SendReceiveClient::new(Some(settings));
    let token = receive
        .request_send_access_token(bitwarden_auth::send_access::SendAccessTokenRequest {
            send_id: access_id.to_owned(),
            send_access_credentials: None,
        })
        .await
        .sdk("send access token")?
        .token;
    let response = receive.access_send(token.clone()).await.sdk("open send")?;
    let view = receive
        .decrypt_send_access(key.to_owned(), response)
        .sdk("decrypt send")?;
    let file_url = match view.file.as_ref().and_then(|f| f.id.clone()) {
        Some(file_id) => {
            receive
                .get_file_download_data(token, file_id)
                .await
                .sdk("send file download")?
                .url
        }
        None => None,
    };
    Ok(ReceivedSend {
        name: view.name,
        text: view.text.and_then(|t| t.text),
        file_name: view.file.and_then(|f| f.file_name),
        file_url,
    })
}

/// Decrypts a downloaded Send file.
pub fn decrypt_send_file(key: &str, data: &[u8]) -> Result<Vec<u8>, Error> {
    bitwarden_send::SendAccessKey::from_url_b64(key)
        .sdk("send key")?
        .decrypt_file_buffer(data)
        .sdk("decrypt send file")
}

fn not_found(id: &str) -> Error {
    Error::NotFound(format!("no Send with id {id:?}"))
}

fn info(view: &SendView) -> Option<SendInfo> {
    let file = view.file.as_ref();
    Some(SendInfo {
        id: view.id?.to_string(),
        name: view.name.clone(),
        file_name: if view.r#type == SendType::File {
            file.map(|f| f.file_name.clone())
        } else {
            None
        },
        file_size: file.and_then(|f| f.size_name.clone()),
        deletion_date: view.deletion_date.to_rfc3339(),
        expiration_date: view.expiration_date.map(|d| d.to_rfc3339()),
        access_count: view.access_count,
        max_access_count: view.max_access_count,
        disabled: view.disabled,
        has_password: view.has_password,
        hide_email: view.hide_email,
        notes: view.notes.clone().filter(|n| !n.is_empty()),
    })
}

/// The request for `body`, without the options that have a default.
fn add_request(
    name: String,
    notes: Option<String>,
    body: &SendBody,
    deletion_days: u32,
) -> Result<SendAddRequest, Error> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err(Error::Invalid("a Send needs a name".into()));
    }
    if !(1..=MAX_SEND_DAYS).contains(&deletion_days) {
        return Err(Error::Invalid(format!(
            "a Send is deleted after 1 to {MAX_SEND_DAYS} days"
        )));
    }
    let view_type = match body {
        SendBody::Text { text, hidden } => {
            if text.is_empty() {
                return Err(Error::Invalid("the Send has no text".into()));
            }
            SendViewType::Text(SendTextView {
                text: Some(text.to_string()),
                hidden: *hidden,
            })
        }
        SendBody::File { name, data } => {
            if data.is_empty() {
                return Err(Error::Invalid("the file is empty".into()));
            }
            if data.len() > MAX_SEND_FILE {
                return Err(Error::Invalid(format!(
                    "the file is larger than {} MiB",
                    MAX_SEND_FILE / 1024 / 1024
                )));
            }
            SendViewType::File(SendFileView {
                id: None,
                file_name: name.clone(),
                size: None,
                size_name: None,
            })
        }
    };
    Ok(SendAddRequest {
        name,
        notes: notes.filter(|n| !n.trim().is_empty()),
        view_type,
        max_access_count: None,
        disabled: false,
        hide_email: false,
        deletion_date: chrono::Utc::now() + chrono::Duration::days(i64::from(deletion_days)),
        expiration_date: None,
        auth: SendAuthType::None,
    })
}

/// The share link: the web vault's Send page, the Send's access id, and its key.
fn link(web_vault: &str, view: &SendView) -> Result<Zeroizing<String>, Error> {
    let (Some(access_id), Some(key)) = (&view.access_id, &view.key) else {
        return Err(Error::State("the Send has no access id or key".into()));
    };
    Ok(Zeroizing::new(format!(
        "{web_vault}/#/send/{access_id}/{key}"
    )))
}

/// bitwarden.com's API is at `api.<domain>` and its web vault at `vault.<domain>`; a
/// self-hosted server has both under one base URL (API at `<base>/api`).
fn web_vault(api_url: &str) -> String {
    let api_url = api_url.trim_end_matches('/');
    if let Some(domain) = api_url.strip_prefix("https://api.") {
        return format!("https://vault.{domain}");
    }
    api_url.strip_suffix("/api").unwrap_or(api_url).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_links() {
        assert_eq!(
            parse_send_link("https://vault.bitwarden.com/#/send/AbC/k-E_y"),
            Some(("AbC", "k-E_y"))
        );
        assert_eq!(parse_send_link("https://example.test/"), None);
        let cloud = server_of_link("https://vault.bitwarden.eu/#/send/a/b").unwrap();
        assert_eq!(cloud.api_url, "https://api.bitwarden.eu");
        let own = server_of_link("http://127.0.0.1:8222/#/send/a/b").unwrap();
        assert_eq!(own.identity_url, "http://127.0.0.1:8222/identity");
    }

    #[test]
    fn finds_the_web_vault() {
        assert_eq!(
            web_vault("https://api.bitwarden.com"),
            "https://vault.bitwarden.com"
        );
        assert_eq!(
            web_vault("https://api.bitwarden.eu/"),
            "https://vault.bitwarden.eu"
        );
        assert_eq!(
            web_vault("http://127.0.0.1:8222/api"),
            "http://127.0.0.1:8222"
        );
        assert_eq!(
            web_vault("https://vault.example.test/api"),
            "https://vault.example.test"
        );
    }

    #[test]
    fn checks_new_sends() {
        let text = |t: &str| SendBody::Text {
            text: Zeroizing::new(t.to_owned()),
            hidden: false,
        };
        assert!(add_request("x".into(), None, &text("hi"), 7).is_ok());
        assert!(add_request(" ".into(), None, &text("hi"), 7).is_err());
        assert!(add_request("x".into(), None, &text(""), 7).is_err());
        assert!(add_request("x".into(), None, &text("hi"), 0).is_err());
        assert!(add_request("x".into(), None, &text("hi"), 32).is_err());
        let big = SendBody::File {
            name: "a.bin".into(),
            data: Zeroizing::new(vec![0; MAX_SEND_FILE + 1]),
        };
        assert!(add_request("x".into(), None, &big, 7).is_err());
        let request = add_request("x".into(), Some(" ".into()), &text("hi"), 2).unwrap();
        assert_eq!(request.notes, None);
        let days = (request.deletion_date - chrono::Utc::now()).num_hours();
        assert!((47..=48).contains(&days));
    }
}
