//! Where stored values live.

use std::collections::HashMap;

use parking_lot::Mutex;

/// A store that couldn't be read or written. The message is for logs; the
/// admin page shows a generic failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct StoreError(pub String);

impl StoreError {
    pub fn new(message: impl std::fmt::Display) -> Self {
        StoreError(message.to_string())
    }
}

/// Where stored values live: each process's own `settings` table.
///
/// Async so an implementation over a database does its I/O off the async
/// worker threads (on its database threads, or `spawn_blocking`): a save
/// that waits behind a long write must not stall a worker.
#[async_trait::async_trait]
pub trait SettingsStore: Send + Sync {
    /// Every stored key and its raw value.
    async fn read_all(&self) -> Result<HashMap<String, String>, StoreError>;

    /// Applies every change in one transaction: all of them or none.
    /// `Some` sets a key, `None` deletes it (so the setting goes back to
    /// its default).
    async fn write_all(
        &self,
        changes: Vec<(&'static str, Option<String>)>,
    ) -> Result<(), StoreError>;
}

/// A store in memory, for tests and tools.
#[derive(Debug, Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        MemoryStore::default()
    }

    /// A store that already holds `values`.
    pub fn with<K: Into<String>, V: Into<String>>(
        values: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        MemoryStore {
            values: Mutex::new(
                values
                    .into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
            ),
        }
    }

    /// One stored value.
    pub fn get(&self, key: &str) -> Option<String> {
        self.values.lock().get(key).cloned()
    }
}

#[async_trait::async_trait]
impl SettingsStore for MemoryStore {
    async fn read_all(&self) -> Result<HashMap<String, String>, StoreError> {
        Ok(self.values.lock().clone())
    }

    async fn write_all(
        &self,
        changes: Vec<(&'static str, Option<String>)>,
    ) -> Result<(), StoreError> {
        let mut values = self.values.lock();
        for (key, value) in changes {
            match value {
                Some(value) => values.insert(key.to_string(), value),
                None => values.remove(key),
            };
        }
        Ok(())
    }
}
