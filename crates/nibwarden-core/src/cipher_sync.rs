use std::sync::{Arc, Mutex};

use bitwarden_api_api::models::SyncResponseModel;
use bitwarden_core::key_management::MasterPasswordUnlockData;
use bitwarden_state::repository::Repository;
use bitwarden_sync::{SyncHandler, SyncHandlerError};
use bitwarden_vault::{Cipher, CipherId};

/// Persists ciphers from a sync into SDK-managed storage.
///
/// sdk-internal ships sync handlers for folders, sends and policies but not ciphers
/// (the TypeScript clients store those themselves), so `vault().ciphers().list()` would
/// stay empty without this.
pub(crate) struct CipherSyncHandler {
    pub repository: Arc<dyn Repository<Cipher>>,
}

#[async_trait::async_trait]
impl SyncHandler for CipherSyncHandler {
    async fn on_sync(&self, response: &SyncResponseModel) -> Result<(), SyncHandlerError> {
        let Some(api_ciphers) = response.ciphers.as_ref() else {
            return Ok(());
        };

        let ciphers: Vec<(CipherId, Cipher)> = api_ciphers
            .iter()
            .filter_map(|c| {
                Cipher::try_from(c.clone())
                    .inspect_err(|e| tracing::error!(id = ?c.id, error = ?e, "skipping cipher"))
                    .ok()
                    .and_then(|cipher| Some((cipher.id?, cipher)))
            })
            .collect();

        self.repository.replace_all(ciphers).await?;
        Ok(())
    }
}

/// Captures the master-password unlock data from each sync. The server is the source of
/// truth for KDF settings, which can change (e.g. after a KDF upgrade on another client).
#[derive(Default)]
pub(crate) struct UnlockDataCapture {
    pub latest: Mutex<Option<MasterPasswordUnlockData>>,
}

#[async_trait::async_trait]
impl SyncHandler for UnlockDataCapture {
    async fn on_sync(&self, response: &SyncResponseModel) -> Result<(), SyncHandlerError> {
        let data = response
            .user_decryption
            .as_ref()
            .and_then(|d| d.master_password_unlock.as_deref())
            .and_then(|m| {
                MasterPasswordUnlockData::try_from(m)
                    .inspect_err(|e| tracing::error!(error = ?e, "bad master password unlock data"))
                    .ok()
            });
        *self.latest.lock().expect("poisoned") = data;
        Ok(())
    }
}
