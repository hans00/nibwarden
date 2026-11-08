use std::{collections::BTreeMap, marker::PhantomData, path::PathBuf};

use bitwarden_state::repository::{Repository, RepositoryError, RepositoryItem};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::Mutex;

use crate::store::write_private;

/// A JSON-file-backed repository for the SDK's *client-managed* items, i.e. the ones the
/// SDK expects the host application to store (they are absent from
/// `get_sdk_managed_migrations`). Meant for small item sets only: every write rewrites the
/// whole file.
pub(crate) struct FileRepository<V> {
    path: PathBuf,
    lock: Mutex<()>,
    _item: PhantomData<fn() -> V>,
}

impl<V> FileRepository<V> {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
            _item: PhantomData,
        }
    }
}

impl<V: Serialize + DeserializeOwned> FileRepository<V> {
    fn load(&self) -> Result<BTreeMap<String, V>, RepositoryError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(internal),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(internal(e)),
        }
    }

    fn save(&self, map: &BTreeMap<String, V>) -> Result<(), RepositoryError> {
        let bytes = serde_json::to_vec(map).map_err(internal)?;
        write_private(&self.path, &bytes).map_err(internal)
    }
}

fn internal(e: impl std::fmt::Display) -> RepositoryError {
    RepositoryError::Internal(e.to_string())
}

#[async_trait::async_trait]
impl<V> Repository<V> for FileRepository<V>
where
    V: RepositoryItem + Serialize + DeserializeOwned,
{
    async fn get(&self, key: V::Key) -> Result<Option<V>, RepositoryError> {
        let _guard = self.lock.lock().await;
        Ok(self.load()?.remove(&key.to_string()))
    }

    async fn list(&self) -> Result<Vec<V>, RepositoryError> {
        let _guard = self.lock.lock().await;
        Ok(self.load()?.into_values().collect())
    }

    async fn set(&self, key: V::Key, value: V) -> Result<(), RepositoryError> {
        self.set_bulk(vec![(key, value)]).await
    }

    async fn set_bulk(&self, values: Vec<(V::Key, V)>) -> Result<(), RepositoryError> {
        let _guard = self.lock.lock().await;
        let mut map = self.load()?;
        map.extend(values.into_iter().map(|(k, v)| (k.to_string(), v)));
        self.save(&map)
    }

    async fn remove(&self, key: V::Key) -> Result<(), RepositoryError> {
        self.remove_bulk(vec![key]).await
    }

    async fn remove_bulk(&self, keys: Vec<V::Key>) -> Result<(), RepositoryError> {
        let _guard = self.lock.lock().await;
        let mut map = self.load()?;
        for key in keys {
            map.remove(&key.to_string());
        }
        self.save(&map)
    }

    async fn remove_all(&self) -> Result<(), RepositoryError> {
        let _guard = self.lock.lock().await;
        self.save(&BTreeMap::new())
    }
}
