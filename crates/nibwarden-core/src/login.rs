//! The password login request (phase2.md §5.7), sent by nibwarden instead of the SDK.
//!
//! The SDK's password login names every client "Chrome", device "firefox", with one fixed
//! device identifier shared by all SDK clients, so the server's sign-in emails and device
//! list show an unknown browser. It also cannot send `newDeviceOtp`. So the token request
//! is made here, as the desktop app with this data directory's device id, and the answer
//! is put into the SDK's state the way its own login does.
//!
//! New device verification: Bitwarden asks an account without two-step login to confirm a
//! device it has not seen with a code it emails (`device_error`); the code goes back as
//! `newDeviceOtp` with the same request.

use bitwarden_api_api::models::MasterPasswordUnlockResponseModel;
use bitwarden_core::{
    ClientBuilder, UserId,
    auth::JwtToken,
    client::{
        login_method::UserLoginMethod,
        persisted_state::{AUTHENTICATION_TOKENS, AuthenticationTokens, USER_LOGIN_METHOD},
    },
    get_host_platform_info,
    key_management::{
        MasterPasswordAuthenticationData, MasterPasswordUnlockData,
        account_cryptographic_state::WrappedAccountCryptographicState,
        crypto::{InitUserCryptoMethod, InitUserCryptoRequest},
    },
};
use serde_json::Value;

use crate::{Error, Server, TwoFactor, TwoFactorProvider, error::SdkContext as _};

/// The OAuth client of Bitwarden's desktop app, which matches the device type sent
/// (`platform::DEVICE_TYPE`). Token refreshes use it too, through the saved login method.
const CLIENT_ID: &str = "desktop";
/// Shown in the server's sign-in emails and device list.
const DEVICE_NAME: &str = "nibwarden";

const WRONG_CODE: &str = "The verification code is invalid. Try again.";

/// Logs in on `client` (a client built on an empty state): requests tokens and saves them
/// with the login method in the client's state, and unlocks the client's keys. Two-step
/// login and new device verification come back as [`Error::TwoFactorRequired`].
pub(crate) async fn password(
    client: &bitwarden_core::Client,
    email: &str,
    password: &str,
    two_factor: Option<&TwoFactor>,
    device_id: &str,
) -> Result<UserId, Error> {
    let kdf = client
        .auth()
        .prelogin(email.to_owned())
        .await
        .sdk("ask the server how to derive the key")?;
    let hash = MasterPasswordAuthenticationData::derive(password, &kdf, email)
        .sdk("derive the master key")?
        .master_password_authentication_hash
        .to_string();

    // Finished before the request is sent: the serializer is not `Send`.
    let body = {
        let mut form = form_urlencoded::Serializer::new(String::new());
        form.append_pair("scope", "api offline_access")
            .append_pair("client_id", CLIENT_ID)
            .append_pair(
                "deviceType",
                &(crate::platform::DEVICE_TYPE as u8).to_string(),
            )
            .append_pair("deviceIdentifier", device_id)
            .append_pair("deviceName", DEVICE_NAME)
            .append_pair("grant_type", "password")
            .append_pair("username", email)
            .append_pair("password", &hash);
        match two_factor {
            Some(TwoFactor {
                provider: TwoFactorProvider::NewDevice,
                token,
            }) => {
                form.append_pair("newDeviceOtp", token.trim());
            }
            Some(TwoFactor { provider, token }) => {
                let provider = match provider {
                    TwoFactorProvider::Authenticator => "0",
                    _ => "1",
                };
                form.append_pair("twoFactorToken", token.trim())
                    .append_pair("twoFactorProvider", provider)
                    .append_pair("twoFactorRemember", "false");
            }
            None => {}
        }
        form.finish()
    };

    let identity = &client.internal.get_api_configurations().identity_config;
    let response = identity
        .client
        .post(format!("{}/connect/token", identity.base_path))
        .header(
            "Content-Type",
            "application/x-www-form-urlencoded; charset=utf-8",
        )
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await
        .sdk("reach the server")?;
    let status = response.status();
    let text = response.text().await.sdk("read the server's answer")?;
    let body: Value = serde_json::from_str(&text).unwrap_or_default();
    if !status.is_success() {
        return Err(two_factor_required(&body).unwrap_or_else(|| identity_error(&body)));
    }

    let field = |camel: &str, pascal: &str| body.get(camel).or_else(|| body.get(pascal));
    let access_token = field("access_token", "access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::State("the server's login answer has no access token".into()))?;
    let user_id: UserId = access_token
        .parse::<JwtToken>()
        .sdk("parse access token")?
        .sub
        .parse()
        .sdk("parse user id")?;
    let private_key = field("privateKey", "PrivateKey")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::State("the server's login answer has no account keys".into()))?
        .parse()
        .sdk("read account keys")?;
    let unlock = field("userDecryptionOptions", "UserDecryptionOptions")
        .and_then(|o| {
            o.get("masterPasswordUnlock")
                .or_else(|| o.get("MasterPasswordUnlock"))
        })
        .cloned()
        .map(serde_json::from_value::<MasterPasswordUnlockResponseModel>)
        .transpose()
        .sdk("read the master password unlock data")?
        .ok_or_else(|| {
            Error::State("the server did not send master password unlock data".into())
        })?;
    let unlock =
        MasterPasswordUnlockData::try_from(&unlock).sdk("read the master password unlock data")?;

    let state = client.platform().state();
    state
        .setting(AUTHENTICATION_TOKENS)
        .sdk("save tokens")?
        .update(AuthenticationTokens {
            access_token: access_token.to_owned(),
            refresh_token: field("refresh_token", "refresh_token")
                .and_then(Value::as_str)
                .map(str::to_owned),
            expires_on: chrono::Utc::now().timestamp()
                + field("expires_in", "expires_in")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
        })
        .await
        .sdk("save tokens")?;

    client
        .crypto()
        .initialize_user_crypto(InitUserCryptoRequest {
            user_id: Some(user_id),
            kdf_params: unlock.kdf.clone(),
            email: email.to_owned(),
            account_cryptographic_state: WrappedAccountCryptographicState::V1 { private_key },
            method: InitUserCryptoMethod::MasterPasswordUnlock {
                password: password.to_owned(),
                master_password_unlock: unlock,
            },
            upgrade_token: None,
        })
        .await
        .sdk("unlock the account keys")?;
    // Unlocking the keys saved the login method, without the client.
    keep_client_id(client.platform()).await?;
    Ok(user_id)
}

/// The SDK's unlock saves the login method again with an empty OAuth client, and the
/// server refuses a token refresh without one (`invalid_request`), so sync would stop an
/// hour after unlocking. Put back after every unlock.
pub(crate) async fn keep_client_id(
    platform: bitwarden_core::platform::PlatformClient,
) -> Result<(), Error> {
    let setting = platform
        .state()
        .setting(USER_LOGIN_METHOD)
        .sdk("read login method")?;
    if let Some(UserLoginMethod::Username {
        client_id,
        email,
        kdf,
    }) = setting.get().await.sdk("read login method")?
        && client_id != CLIENT_ID
    {
        setting
            .update(UserLoginMethod::Username {
                client_id: CLIENT_ID.to_owned(),
                email,
                kdf,
            })
            .await
            .sdk("save login method")?;
    }
    Ok(())
}

/// Asks the server to email a new code for confirming this device.
pub(crate) async fn resend_new_device_code(
    server: &Server,
    email: &str,
    password: &str,
) -> Result<(), Error> {
    let settings = get_host_platform_info()
        .to_client_settings(server.api_url.clone(), server.identity_url.clone());
    let client = ClientBuilder::new().with_settings(settings).build();
    let kdf = client
        .auth()
        .prelogin(email.to_owned())
        .await
        .sdk("ask the server how to derive the key")?;
    let hash = MasterPasswordAuthenticationData::derive(password, &kdf, email)
        .sdk("derive the master key")?
        .master_password_authentication_hash
        .to_string();
    // The code is scoped to the device in the `Device-Identifier` header, which the client
    // sends as the login request's `deviceIdentifier`.
    let identity = &client.internal.get_api_configurations().identity_config;
    let response = identity
        .client
        .post(format!("{}/accounts/resend-new-device-otp", server.api_url))
        .json(&serde_json::json!({ "email": email, "secret": hash }))
        .send()
        .await
        .sdk("reach the server")?;
    if response.status().is_success() {
        return Ok(());
    }
    let text = response.text().await.sdk("read the server's answer")?;
    Err(identity_error(
        &serde_json::from_str(&text).unwrap_or_default(),
    ))
}

/// The two-step methods a refusal asks for, of those nibwarden supports.
fn two_factor_required(body: &Value) -> Option<Error> {
    let providers = body
        .get("TwoFactorProviders2")
        .or_else(|| body.get("twoFactorProviders2"))?
        .as_object()?;
    let mut usable = Vec::new();
    if providers.contains_key("0") {
        usable.push(TwoFactorProvider::Authenticator);
    }
    if providers.contains_key("1") {
        usable.push(TwoFactorProvider::Email);
    }
    if usable.is_empty() {
        let offered: Vec<_> = providers.keys().map(String::as_str).collect();
        return Some(Error::State(format!(
            "this account only offers two-step methods nibwarden does not support yet \
             (Bitwarden provider types {})",
            offered.join(", ")
        )));
    }
    Some(Error::TwoFactorRequired(usable))
}

/// What a refusal from the identity server means: a device to verify, a wrong code, or
/// [`Error::Rejected`] with the server's own explanation.
pub(crate) fn identity_error(body: &Value) -> Error {
    if body.get("error").and_then(|e| e.as_str()) == Some("device_error") {
        let description = body
            .get("error_description")
            .and_then(|d| d.as_str())
            .unwrap_or_default();
        // The server's comments call these strings the contract with its clients.
        if description.eq_ignore_ascii_case("New device verification required") {
            return Error::TwoFactorRequired(vec![TwoFactorProvider::NewDevice]);
        }
        if description.eq_ignore_ascii_case("Invalid New Device OTP") {
            return Error::Rejected(WRONG_CODE.to_owned());
        }
    }
    let message = [
        "/errorModel/message",
        "/ErrorModel/Message",
        "/error_model/message",
        "/message",
        "/error_description",
    ]
    .iter()
    .find_map(|pointer| body.pointer(pointer)?.as_str().filter(|m| !m.is_empty()))
    .unwrap_or("invalid credentials");
    Error::Rejected(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_device_errors() {
        let required = serde_json::json!({
            "error": "device_error",
            "error_description": "New device verification required",
            "ErrorModel": { "Message": "new device verification required", "Object": "error" },
        });
        assert!(matches!(
            identity_error(&required),
            Error::TwoFactorRequired(p) if p == [TwoFactorProvider::NewDevice]
        ));
        let wrong = serde_json::json!({
            "error": "device_error",
            "error_description": "Invalid New Device OTP",
            "ErrorModel": { "Message": "invalid new device otp", "Object": "error" },
        });
        assert!(matches!(identity_error(&wrong), Error::Rejected(m) if m == WRONG_CODE));
        let password = serde_json::json!({
            "error": "invalid_grant",
            "error_description": "invalid_username_or_password",
            "ErrorModel": { "Message": "Username or password is incorrect. Try again.", "Object": "error" },
        });
        assert!(matches!(
            identity_error(&password),
            Error::Rejected(m) if m == "Username or password is incorrect. Try again."
        ));
    }

    #[test]
    fn reads_two_step_requests() {
        let both = serde_json::json!({
            "error": "invalid_grant",
            "error_description": "Two factor required.",
            "TwoFactorProviders2": { "0": null, "1": { "Email": "t***@example.com" } },
        });
        assert!(matches!(
            two_factor_required(&both),
            Some(Error::TwoFactorRequired(p))
                if p == [TwoFactorProvider::Authenticator, TwoFactorProvider::Email]
        ));
        let webauthn = serde_json::json!({ "TwoFactorProviders2": { "7": {} } });
        assert!(matches!(
            two_factor_required(&webauthn),
            Some(Error::State(_))
        ));
        assert!(two_factor_required(&serde_json::json!({ "error": "invalid_grant" })).is_none());
    }
}
