use bitwarden_core::{ClientBuilder, get_host_platform_info};
use bitwarden_crypto::Kdf;
use bitwarden_pm::PasswordManagerClient;

use crate::{Error, Server, error::SdkContext as _};

/// PBKDF2 iteration count the official clients use for new accounts.
const PBKDF2_ITERATIONS: u32 = 600_000;

/// Key material for registering a new account.
pub struct Registration {
    pub master_password_hash: String,
    pub encrypted_user_key: String,
    pub public_key: String,
    pub encrypted_private_key: String,
    pub kdf_iterations: u32,
}

pub fn make_registration(email: &str, password: &str) -> Result<Registration, Error> {
    let kdf = Kdf::PBKDF2 {
        iterations: PBKDF2_ITERATIONS.try_into().expect("non-zero"),
    };
    // Key derivation needs no server, but the SDK only exposes it on a client instance.
    let keys = PasswordManagerClient::new(None)
        .0
        .auth()
        .make_register_keys(email.to_owned(), password.to_owned(), kdf)
        .sdk("derive registration keys")?;
    Ok(Registration {
        master_password_hash: keys.master_password_hash.to_string(),
        encrypted_user_key: keys.encrypted_user_key.to_string(),
        public_key: keys.keys.public.to_string(),
        encrypted_private_key: keys.keys.private.to_string(),
        kdf_iterations: PBKDF2_ITERATIONS,
    })
}

/// An email address as the official clients use it: the master key is salted with it, so
/// signing up and logging in must spell it the same way.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// Starts creating an account. The server either emails a link to finish with (`None`),
/// or, when it does not verify addresses (a self-hosted server without mail), hands back
/// the token to finish with right away.
pub async fn start_registration(
    server: &Server,
    email: &str,
    name: &str,
) -> Result<Option<String>, Error> {
    let email = normalize_email(email);
    let name = name.trim();
    let body = serde_json::json!({
        "email": email,
        "name": if name.is_empty() { None } else { Some(name) },
        "receiveMarketingEmails": false,
    });
    // The SDK's own call for this drops the response body, which is where the token is.
    let text = post_identity(server, "/accounts/register/send-verification-email", &body).await?;
    let token = serde_json::from_str::<String>(&text).unwrap_or(text);
    let token = token.trim();
    Ok((!token.is_empty()).then(|| token.to_owned()))
}

/// Creates the account with the token from [`start_registration`] or from the emailed
/// link. Log in afterwards as usual.
pub async fn finish_registration(
    server: &Server,
    email: &str,
    token: &str,
    password: &str,
    hint: Option<&str>,
) -> Result<(), Error> {
    let email = normalize_email(email);
    let reg = make_registration(&email, password)?;
    let body = serde_json::json!({
        "email": email,
        "emailVerificationToken": token.trim(),
        "masterPasswordHash": reg.master_password_hash,
        "masterPasswordHint": hint.map(str::trim).filter(|h| !h.is_empty()),
        "userSymmetricKey": reg.encrypted_user_key,
        "userAsymmetricKeys": {
            "publicKey": reg.public_key,
            "encryptedPrivateKey": reg.encrypted_private_key,
        },
        "kdf": 0,
        "kdfIterations": reg.kdf_iterations,
    });
    post_identity(server, "/accounts/register/finish", &body).await?;
    Ok(())
}

/// POSTs to the identity service through the SDK's HTTP client (its TLS settings and
/// client headers); the response body on success, the server's explanation otherwise.
async fn post_identity(
    server: &Server,
    path: &str,
    body: &serde_json::Value,
) -> Result<String, Error> {
    let settings = get_host_platform_info()
        .to_client_settings(server.api_url.clone(), server.identity_url.clone());
    let client = ClientBuilder::new().with_settings(settings).build();
    let config = client.internal.get_api_configurations();
    let identity = &config.identity_config;
    let response = identity
        .client
        .post(format!("{}{path}", identity.base_path))
        .json(body)
        .send()
        .await
        .sdk("reach the server")?;
    let status = response.status();
    let text = response.text().await.sdk("read the server's answer")?;
    if status.is_success() {
        Ok(text)
    } else {
        Err(Error::Rejected(
            server_message(&text).unwrap_or_else(|| format!("HTTP {}", status.as_u16())),
        ))
    }
}

/// The reason in an error body: Bitwarden puts the useful part in `validationErrors`,
/// Vaultwarden in `message`.
fn server_message(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let validation = value
        .get("validationErrors")
        .or_else(|| value.get("ValidationErrors"))
        .and_then(|v| v.as_object())
        .and_then(|errors| errors.values().find_map(|v| v.get(0)?.as_str()));
    let message = value
        .get("message")
        .or_else(|| value.get("Message"))
        .and_then(|m| m.as_str());
    validation
        .or(message)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_server_messages() {
        let bitwarden = r#"{"message":"The model state is invalid.","validationErrors":{"Email":["Email 'a@b.c' is already taken."]}}"#;
        assert_eq!(
            server_message(bitwarden).as_deref(),
            Some("Email 'a@b.c' is already taken.")
        );
        let vaultwarden = r#"{"message":"Registration not allowed or user already exists","validationErrors":{"":["Registration not allowed or user already exists"]}}"#;
        assert_eq!(
            server_message(vaultwarden).as_deref(),
            Some("Registration not allowed or user already exists")
        );
        assert_eq!(
            server_message(r#"{"message":"Nope"}"#).as_deref(),
            Some("Nope")
        );
        assert_eq!(server_message("<html>"), None);
    }

    #[test]
    fn normalizes_emails() {
        assert_eq!(normalize_email("  Alice@Example.COM "), "alice@example.com");
    }
}
