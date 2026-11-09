use std::sync::Arc;

use bitwarden_auth::token_management::PasswordManagerTokenHandler;
use bitwarden_core::{
    ClientBuilder, FromClient as _,
    auth::login::TwoFactorEmailRequest,
    client::encryption_settings::EncryptionSettingsError,
    client::persisted_state::{ACCOUNT_CRYPTO_STATE, AUTHENTICATION_TOKENS, BASE_URLS, BaseUrls},
    get_host_platform_info,
    key_management::{
        account_cryptographic_state::WrappedAccountCryptographicState,
        crypto::{InitUserCryptoMethod, InitUserCryptoRequest},
    },
};
use bitwarden_pm::{PasswordManagerClient, SaveStateData, UnlockError, UnlockMethod};
use bitwarden_send::SendSyncHandler;
use bitwarden_sync::{SyncClient, SyncRequest};
use bitwarden_vault::{Cipher, FolderSyncHandler};

use crate::{
    Error, Server, Store,
    cipher_sync::{CipherSyncHandler, UnlockDataCapture},
    error::SdkContext as _,
    login,
    store::UnlockData,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwoFactorProvider {
    Authenticator,
    Email,
    /// Not two-step login, but asked for the same way: the code Bitwarden emails to
    /// confirm a device it does not know yet (`crate::login`).
    NewDevice,
}

#[derive(Debug, Clone)]
pub struct TwoFactor {
    pub provider: TwoFactorProvider,
    pub token: String,
}

/// One logged-in Bitwarden account backed by a [`Store`].
pub struct Account {
    pub(crate) client: PasswordManagerClient,
    // `client.sync()` builds a fresh SyncClient with an empty handler registry on every
    // call, so the instance our handlers are registered on must be kept and reused.
    sync: SyncClient,
    store: Store,
    unlock_capture: Arc<UnlockDataCapture>,
    user_id: String,
    email: String,
    /// What unlocks this session besides the master password, in memory only.
    quick: std::sync::Mutex<QuickUnlock>,
}

/// Unlocking without the master password, after it unlocked once (phase2.md §12.19).
#[derive(Default)]
struct QuickUnlock {
    /// The user key protected by the PIN.
    pin: Option<bitwarden_crypto::safe::PasswordProtectedKeyEnvelope>,
    /// The user key itself, for system authentication.
    key: Option<zeroize::Zeroizing<String>>,
}

impl Account {
    /// Logs in with email + master password, persists the session, and performs a first
    /// full sync. Any previous session in `store` is wiped. The returned account is unlocked.
    pub async fn login_password(
        store: Store,
        server: &Server,
        email: &str,
        password: &str,
        two_factor: Option<TwoFactor>,
    ) -> Result<Self, Error> {
        store.open_registry().await?.wipe().await.ok();
        store.remove_local_files()?;

        // `wipe` leaves its registry unusable, so the login client gets a fresh one.
        let registry = store.open_registry().await?;
        let settings = get_host_platform_info()
            .to_client_settings(server.api_url.clone(), server.identity_url.clone());
        let client = PasswordManagerClient(
            ClientBuilder::new()
                .with_settings(settings)
                .with_token_handler(Arc::new(PasswordManagerTokenHandler::default()))
                .with_state(registry)
                .build(),
        );

        let user_id = login::password(
            &client.0,
            email,
            password,
            two_factor.as_ref(),
            &store.device_id()?,
        )
        .await?;

        let crypto_state = {
            let key_store = client.0.internal.get_key_store();
            WrappedAccountCryptographicState::get_from_key_store(&key_store.context())
                .sdk("read account keys")?
        };
        let save_registry = store.open_registry().await?;
        PasswordManagerClient::save_to_state(
            SaveStateData {
                user_id,
                email: email.to_owned(),
                urls: BaseUrls {
                    api_url: server.api_url.clone(),
                    identity_url: server.identity_url.clone(),
                },
                crypto_state,
            },
            &save_registry,
        )
        .await
        .sdk("save session")?;

        let account = Self::attach(client, store, user_id.to_string(), email.to_owned())?;
        account.sync(true).await?;
        Ok(account)
    }

    /// Asks the server to email a two-step login code. Needed on bitwarden.com; some
    /// self-hosted servers send one on their own, and a new request replaces that code.
    pub async fn send_two_factor_email(
        server: &Server,
        email: &str,
        password: &str,
    ) -> Result<(), Error> {
        let settings = get_host_platform_info()
            .to_client_settings(server.api_url.clone(), server.identity_url.clone());
        let client = ClientBuilder::new().with_settings(settings).build();
        client
            .auth()
            .send_two_factor_email(&TwoFactorEmailRequest {
                password: password.to_owned(),
                email: email.to_owned(),
            })
            .await
            .sdk("send two-step login email")
    }

    /// Asks the server to email a new code for confirming this device.
    pub async fn send_new_device_code(
        server: &Server,
        email: &str,
        password: &str,
    ) -> Result<(), Error> {
        login::resend_new_device_code(server, email, password).await
    }

    /// Locks, then deletes every trace of the session from the store (the device id is
    /// kept, so the server keeps recognising this device). The account must not be used
    /// afterwards.
    pub async fn logout(&self) -> Result<(), Error> {
        self.lock().await;
        self.store
            .open_registry()
            .await?
            .wipe()
            .await
            .sdk("wipe state")?;
        self.store.remove_local_files()
    }

    /// Restores the persisted session, locked. Returns `None` when logged out.
    pub async fn load(store: Store) -> Result<Option<Self>, Error> {
        let Some(unlock) = store.read_unlock_data()? else {
            return Ok(None);
        };
        let registry = store.open_registry().await?;
        let client = PasswordManagerClient::load_from_state(
            Arc::new(PasswordManagerTokenHandler::default()),
            registry,
        )
        .await
        .sdk("restore session")?;
        Self::attach(client, store, unlock.user_id, unlock.email).map(Some)
    }

    fn attach(
        client: PasswordManagerClient,
        store: Store,
        user_id: String,
        email: String,
    ) -> Result<Self, Error> {
        let repository = client
            .platform()
            .state()
            .get::<Cipher>()
            .sdk("open cipher repository")?;
        let unlock_capture = Arc::new(UnlockDataCapture::default());

        let sync = client.sync();
        // Not registered: bitwarden_crypto_sync_handler::CryptoSyncHandler. At this SDK rev it
        // panics ("StateBridge not registered") outside the TS clients, and is documented as
        // unused until sync moves into the SDK. Without it, server-side key rotation is not
        // picked up until the next login (phase1.md Q6).
        sync.register_sync_handler(Arc::new(FolderSyncHandler::from_client(&client.0)));
        sync.register_sync_handler(Arc::new(CipherSyncHandler { repository }));
        sync.register_sync_handler(Arc::new(SendSyncHandler::from_client(&client.0)));
        sync.register_sync_handler(unlock_capture.clone());

        Ok(Self {
            client,
            sync,
            store,
            unlock_capture,
            user_id,
            email,
            quick: Default::default(),
        })
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The decrypted user key (base64 of its 64 bytes), for the browser extension that
    /// unlocks with it (`crate::BrowserChannel`). Only on an unlocked vault.
    pub async fn user_key(&self) -> Result<zeroize::Zeroizing<String>, Error> {
        if !self.is_unlocked() {
            return Err(Error::Locked);
        }
        let key = self
            .client
            .crypto()
            .get_user_encryption_key()
            .await
            .sdk("read the user key")?;
        Ok(zeroize::Zeroizing::new(key.to_string()))
    }

    /// The API base URL this session talks to.
    pub async fn api_url(&self) -> Result<String, Error> {
        let urls = self
            .client
            .platform()
            .state()
            .setting(BASE_URLS)
            .sdk("read server urls")?
            .get()
            .await
            .sdk("read server urls")?
            .ok_or_else(|| Error::State("no server urls in state".into()))?;
        Ok(urls.api_url)
    }

    /// Where and how to listen for the server's change notifications (phase2.md P2-M5):
    /// the hub's WebSocket URL with a current access token, and this device's id (the
    /// server tags notifications with the device that made the change).
    pub async fn push_endpoint(&self) -> Result<PushEndpoint, Error> {
        let mut tokens = self.tokens().await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        if tokens.expires_on < now + 60 {
            // Any authenticated request refreshes the token; a sync checks the revision
            // date first, so it is cheap when nothing changed.
            self.sync(false).await?;
            tokens = self.tokens().await?;
        }
        Ok(PushEndpoint {
            url: hub_url(&self.api_url().await?),
            access_token: zeroize::Zeroizing::new(tokens.access_token),
            device_id: self.store.device_id()?,
        })
    }

    async fn tokens(
        &self,
    ) -> Result<bitwarden_core::client::persisted_state::AuthenticationTokens, Error> {
        self.client
            .platform()
            .state()
            .setting(AUTHENTICATION_TOKENS)
            .sdk("read tokens")?
            .get()
            .await
            .sdk("read tokens")?
            .ok_or(Error::LoggedOut)
    }

    pub fn is_unlocked(&self) -> bool {
        self.client.is_unlocked()
    }

    /// Returns `true` if a full sync ran, `false` if the server reported no changes.
    pub async fn sync(&self, force: bool) -> Result<bool, Error> {
        let ran = self
            .sync
            .sync(SyncRequest {
                force,
                exclude_subdomains: None,
            })
            .await
            .map_err(|e| match e {
                // As Bitwarden's clients: a refused login (the token could not be renewed)
                // or a deleted account ends the session.
                bitwarden_sync::SyncError::Api(bitwarden_core::ApiError::Response(ref r))
                    if r.status.as_u16() == 401 =>
                {
                    Error::SessionEnded
                }
                bitwarden_sync::SyncError::AccountDeleted => Error::SessionEnded,
                e => Error::Sdk {
                    context: "sync",
                    source: Box::new(e),
                },
            })?;

        let captured = self.unlock_capture.latest.lock().expect("poisoned").take();
        if let Some(master_password_unlock) = captured {
            self.store.write_unlock_data(&UnlockData {
                user_id: self.user_id.clone(),
                email: self.email.clone(),
                master_password_unlock,
            })?;
        }
        Ok(ran)
    }

    pub async fn unlock_with_password(&self, password: &str) -> Result<(), Error> {
        let unlock = self.store.read_unlock_data()?.ok_or(Error::LoggedOut)?;
        let method = InitUserCryptoMethod::MasterPasswordUnlock {
            password: password.to_owned(),
            master_password_unlock: unlock.master_password_unlock.clone(),
        };
        self.unlock_with(method).await?;
        // The PIN works again until the daemon ends.
        if let Some(encrypted_pin) = self.store.read_pin()? {
            match self
                .client
                .crypto()
                .enroll_pin_with_encrypted_pin(encrypted_pin)
            {
                Ok(pin) => {
                    self.quick.lock().expect("poisoned").pin =
                        Some(pin.pin_protected_user_key_envelope);
                }
                Err(e) => tracing::warn!(error = %e, "cannot restore the unlock PIN"),
            }
        }
        Ok(())
    }

    /// Unlocks with `method`; [`Error::WrongPassword`] when its secret does not open the
    /// user key.
    async fn unlock_with(&self, method: InitUserCryptoMethod) -> Result<(), Error> {
        let unlock = self.store.read_unlock_data()?.ok_or(Error::LoggedOut)?;
        let account_cryptographic_state = self
            .client
            .platform()
            .state()
            .setting(ACCOUNT_CRYPTO_STATE)
            .sdk("read account keys")?
            .get()
            .await
            .sdk("read account keys")?
            .ok_or_else(|| Error::State("no account keys in state".into()))?;

        let request = InitUserCryptoRequest {
            user_id: Some(unlock.user_id.parse().sdk("parse user id")?),
            kdf_params: unlock.master_password_unlock.kdf.clone(),
            email: unlock.email,
            account_cryptographic_state,
            method,
            upgrade_token: None,
        };
        match self
            .client
            .unlock()
            .unlock(UnlockMethod::Legacy(request))
            .await
        {
            Ok(()) | Err(UnlockError::AlreadyUnlocked) => {
                login::keep_client_id(self.client.platform()).await
            }
            // The secret's key fails to decrypt the user key.
            Err(UnlockError::EncryptionSettings(
                EncryptionSettingsError::Crypto(_) | EncryptionSettingsError::WrongPin,
            )) => Err(Error::WrongPassword),
            Err(e) => Err(e).sdk("unlock"),
        }
    }

    /// Sets the PIN that unlocks this session from now on (`None`: no PIN). It keeps working
    /// across locks, and after the daemon restarts once the master password unlocked again.
    /// Needs an unlocked vault.
    pub fn set_pin(&self, pin: Option<&str>) -> Result<(), Error> {
        let Some(pin) = pin else {
            self.quick.lock().expect("poisoned").pin = None;
            return self.store.write_pin(None);
        };
        if !self.is_unlocked() {
            return Err(Error::Locked);
        }
        let enrolled = self
            .client
            .crypto()
            .enroll_pin(pin.to_owned())
            .sdk("protect the user key with the PIN")?;
        self.store
            .write_pin(Some(&enrolled.user_key_encrypted_pin.to_string()))?;
        self.quick.lock().expect("poisoned").pin = Some(enrolled.pin_protected_user_key_envelope);
        Ok(())
    }

    /// Whether a PIN is set (it may still need one master password unlock to work).
    pub fn has_pin(&self) -> bool {
        self.store.read_pin().is_ok_and(|p| p.is_some())
    }

    /// Whether the PIN unlocks now.
    pub fn pin_ready(&self) -> bool {
        self.quick.lock().expect("poisoned").pin.is_some()
    }

    /// Stops the PIN working until the master password unlocks again (too many wrong ones).
    pub fn suspend_pin(&self) {
        self.quick.lock().expect("poisoned").pin = None;
    }

    pub async fn unlock_with_pin(&self, pin: &str) -> Result<(), Error> {
        let envelope = self
            .quick
            .lock()
            .expect("poisoned")
            .pin
            .clone()
            .ok_or_else(|| Error::State("the PIN needs the master password first".into()))?;
        self.unlock_with(InitUserCryptoMethod::PinEnvelope {
            pin: pin.to_owned(),
            pin_protected_user_key_envelope: envelope,
        })
        .await
    }

    /// Keeps the user key in memory while locked, for unlocking with the system's
    /// authentication; `false` forgets it. Needs an unlocked vault to keep it.
    pub async fn keep_key(&self, keep: bool) -> Result<(), Error> {
        let key = if keep {
            Some(self.user_key().await?)
        } else {
            None
        };
        self.quick.lock().expect("poisoned").key = key;
        Ok(())
    }

    /// Whether the kept key can unlock (system authentication).
    pub fn key_kept(&self) -> bool {
        self.quick.lock().expect("poisoned").key.is_some()
    }

    /// Unlocks with the kept key, once the system's authentication said yes.
    pub async fn unlock_with_kept_key(&self) -> Result<(), Error> {
        let key = self
            .quick
            .lock()
            .expect("poisoned")
            .key
            .clone()
            .ok_or_else(|| {
                Error::State("unlocking this way needs the master password first".into())
            })?;
        self.unlock_with(InitUserCryptoMethod::DecryptedKey {
            decrypted_user_key: key.to_string(),
        })
        .await
    }

    /// Checks the master password without changing the lock state, for items that ask for
    /// it again (reprompt). Costs one key derivation, like an unlock.
    pub fn verify_password(&self, password: &str) -> Result<bool, Error> {
        let unlock = self.store.read_unlock_data()?.ok_or(Error::LoggedOut)?;
        let data = unlock.master_password_unlock;
        let Ok(master_key) = bitwarden_crypto::MasterKey::derive(password, &data.salt, &data.kdf)
        else {
            return Ok(false);
        };
        Ok(master_key
            .decrypt_user_key(data.master_key_wrapped_user_key)
            .is_ok())
    }

    pub async fn lock(&self) {
        self.client.unlock().lock().await;
    }
}

/// The notifications hub for an API base URL: `<base>/notifications/hub` next to a
/// self-hosted `<base>/api`, `notifications.<domain>/hub` for `api.<domain>`; `ws(s)://`.
fn hub_url(api_url: &str) -> String {
    let api_url = api_url.trim_end_matches('/');
    let http = if let Some(base) = api_url.strip_suffix("/api") {
        format!("{base}/notifications/hub")
    } else if let Some((scheme, rest)) = api_url.split_once("://")
        && let Some(domain) = rest.strip_prefix("api.")
    {
        format!("{scheme}://notifications.{domain}/hub")
    } else {
        format!("{api_url}/notifications/hub")
    };
    if let Some(rest) = http.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        http
    }
}

/// See [`Account::push_endpoint`].
pub struct PushEndpoint {
    pub url: String,
    pub access_token: zeroize::Zeroizing<String>,
    pub device_id: String,
}

/// Turns a refusal from the identity server into [`Error::Rejected`] with the
/// human-readable part of its JSON body, or into the request for a device code.
#[cfg(test)]
mod tests {
    use super::hub_url;

    #[test]
    fn hub_urls() {
        assert_eq!(
            hub_url("https://vault.example.test/api"),
            "wss://vault.example.test/notifications/hub"
        );
        assert_eq!(
            hub_url("http://127.0.0.1:8222/api/"),
            "ws://127.0.0.1:8222/notifications/hub"
        );
        assert_eq!(
            hub_url("https://api.bitwarden.com"),
            "wss://notifications.bitwarden.com/hub"
        );
        assert_eq!(
            hub_url("https://api.bitwarden.eu"),
            "wss://notifications.bitwarden.eu/hub"
        );
    }
}
