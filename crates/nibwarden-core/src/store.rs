use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use bitwarden_core::key_management::{LocalUserDataKeyState, MasterPasswordUnlockData};
use bitwarden_pm::migrations::get_sdk_managed_migrations;
use bitwarden_state::{DatabaseConfiguration, registry::StateRegistry};
use nibwarden_platform::fs::{create_private_dir, create_private_file, restrict_file};
use serde::{Deserialize, Serialize};

use crate::{Error, error::SdkContext as _, file_repository::FileRepository};

const DEVICE_ID_FILE: &str = "device-id";
const UNLOCK_FILE: &str = "unlock.json";
const LOCAL_USER_DATA_KEY_FILE: &str = "local-user-data-key.json";
pub(crate) const PIN_FILE: &str = "pin";
const SDK_DB_NAME: &str = "user";

/// On-disk layout of one nibwarden data directory:
///
/// - `user.sqlite`: SDK state (tokens, wrapped account keys, encrypted vault cache).
/// - `device-id`: stable device identifier, so the server does not see a new device per
///   login.
/// - `unlock.json`: what is needed to unlock with the master password. Nothing secret:
///   KDF settings, salt and the master-key-wrapped user key.
/// - `local-user-data-key.json`: client-managed SDK item, the local-data key wrapped by
///   the user key (encrypts e.g. generator history).
/// - `pin`: the unlock PIN encrypted with the user key, so that unlocking with the master
///   password once makes the PIN work again (as Bitwarden's "require master password on
///   restart"). No use without the user key.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct UnlockData {
    pub user_id: String,
    pub email: String,
    pub master_password_unlock: MasterPasswordUnlockData,
}

impl Store {
    /// Opens (creating if needed, private to the user) a data directory and registers the
    /// platform identity with the SDK.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, Error> {
        let dir = dir.into();
        create_private_dir(&dir)?;
        let store = Self { dir };
        crate::platform::init(&store.device_id()?);
        Ok(store)
    }

    pub(crate) fn read_pin(&self) -> Result<Option<String>, Error> {
        match fs::read_to_string(self.dir.join(PIN_FILE)) {
            Ok(text) => Ok(Some(text.trim().to_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn write_pin(&self, encrypted_pin: Option<&str>) -> Result<(), Error> {
        let path = self.dir.join(PIN_FILE);
        match encrypted_pin {
            Some(pin) => write_private(&path, pin.as_bytes()),
            None => match fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Opens the SDK state registry: the SQLite database for SDK-managed items, plus
    /// file-backed repositories for the items the SDK leaves to the host application.
    pub(crate) async fn open_registry(&self) -> Result<StateRegistry, Error> {
        let registry = StateRegistry::new_with_db(
            DatabaseConfiguration::Sqlite {
                db_name: SDK_DB_NAME.to_owned(),
                folder_path: self.dir.clone(),
            },
            get_sdk_managed_migrations(),
        )
        .await
        .sdk("open state database")?;
        // The SDK creates the database with the process umask; the private directory
        // already keeps other users out, this just avoids relying on that alone.
        for suffix in ["", "-wal", "-shm"] {
            restrict_file(&self.dir.join(format!("{SDK_DB_NAME}.sqlite{suffix}")))?;
        }
        registry.register_client_managed::<LocalUserDataKeyState>(Arc::new(FileRepository::new(
            self.dir.join(LOCAL_USER_DATA_KEY_FILE),
        )));
        Ok(registry)
    }

    pub(crate) fn device_id(&self) -> Result<String, Error> {
        let path = self.dir.join(DEVICE_ID_FILE);
        match fs::read_to_string(&path) {
            Ok(id) => Ok(id.trim().to_owned()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = uuid::Uuid::new_v4().to_string();
                write_private(&path, id.as_bytes())?;
                Ok(id)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn read_unlock_data(&self) -> Result<Option<UnlockData>, Error> {
        match fs::read(self.dir.join(UNLOCK_FILE)) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn write_unlock_data(&self, data: &UnlockData) -> Result<(), Error> {
        write_private(&self.dir.join(UNLOCK_FILE), &serde_json::to_vec(data)?)
    }

    /// Removes the files nibwarden owns (the SDK database is wiped through the registry).
    pub(crate) fn remove_local_files(&self) -> Result<(), Error> {
        for name in [UNLOCK_FILE, LOCAL_USER_DATA_KEY_FILE, PIN_FILE] {
            match fs::remove_file(self.dir.join(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        Ok(())
    }
}

/// Writes via a temp file + rename so a crash never leaves a half-written file, with
/// private to the user from the moment the file exists.
pub(crate) fn write_private(path: &Path, contents: &[u8]) -> Result<(), Error> {
    let tmp = path.with_extension("tmp");
    let mut file = create_private_file(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}
