//! Secret storage through the Linux Secret Service.
//!
//! There is exactly one implementation and it talks to the platform keyring.
//! Onera deliberately ships no file-backed fallback: a mod manager that quietly
//! writes an API key to `~/.config` when the keyring is locked would be worse
//! than one that refuses.
//!
//! `keyring` is synchronous and talks to D-Bus, so every call runs on the
//! blocking pool.

use async_trait::async_trait;
use onera_core::ports::SecretStore;
use onera_core::redact::Secret;
use onera_core::{CoreError, Result};

/// Secret Service collection Onera stores under.
pub const SERVICE: &str = "onera";

/// Secret storage backed by the platform keyring.
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self::new(SERVICE)
    }
}

impl KeyringSecretStore {
    /// Store secrets under a service name.
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, key)
            .map_err(|e| CoreError::SecretStore(format!("cannot address the keyring: {e}")))
    }
}

#[async_trait]
impl SecretStore for KeyringSecretStore {
    async fn get(&self, key: &str) -> Result<Option<Secret>> {
        let entry = self.entry(key)?;
        let result = tokio::task::spawn_blocking(move || entry.get_password())
            .await
            .map_err(|e| CoreError::SecretStore(format!("keyring task failed: {e}")))?;
        match result {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CoreError::SecretStore(e.to_string())),
        }
    }

    async fn set(&self, key: &str, value: &Secret) -> Result<()> {
        let entry = self.entry(key)?;
        let plaintext = value.expose().to_owned();
        tokio::task::spawn_blocking(move || entry.set_password(&plaintext))
            .await
            .map_err(|e| CoreError::SecretStore(format!("keyring task failed: {e}")))?
            .map_err(|e| {
                CoreError::SecretStore(format!(
                    "could not store the secret: {e}. Onera will not fall back to plain text"
                ))
            })
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let entry = self.entry(key)?;
        let result = tokio::task::spawn_blocking(move || entry.delete_credential())
            .await
            .map_err(|e| CoreError::SecretStore(format!("keyring task failed: {e}")))?;
        match result {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CoreError::SecretStore(e.to_string())),
        }
    }

    async fn is_available(&self) -> bool {
        // Probing with a read is the only reliable check: the collection may
        // exist but be locked, which only surfaces on access.
        let Ok(entry) = self.entry("onera.availability-probe") else {
            return false;
        };
        matches!(
            tokio::task::spawn_blocking(move || entry.get_password()).await,
            Ok(Ok(_)) | Ok(Err(keyring::Error::NoEntry))
        )
    }
}

/// An in-memory secret store, for tests and headless CI.
///
/// Never used by a shipped binary: [`crate::Onera`] wires
/// [`KeyringSecretStore`]. It exists so the authentication flow can be tested
/// without a D-Bus session.
#[derive(Debug, Default, Clone)]
pub struct InMemorySecretStore {
    entries: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    available: bool,
}

impl InMemorySecretStore {
    /// A working store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Default::default(),
            available: true,
        }
    }

    /// A store that reports itself unavailable, to test the no-fallback rule.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            entries: Default::default(),
            available: false,
        }
    }
}

#[async_trait]
impl SecretStore for InMemorySecretStore {
    async fn get(&self, key: &str) -> Result<Option<Secret>> {
        Ok(self
            .entries
            .lock()
            .expect("secret mutex poisoned")
            .get(key)
            .map(Secret::new))
    }

    async fn set(&self, key: &str, value: &Secret) -> Result<()> {
        if !self.available {
            return Err(CoreError::SecretStore("store is unavailable".into()));
        }
        self.entries
            .lock()
            .expect("secret mutex poisoned")
            .insert(key.to_owned(), value.expose().to_owned());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.entries
            .lock()
            .expect("secret mutex poisoned")
            .remove(key);
        Ok(())
    }

    async fn is_available(&self) -> bool {
        self.available
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn secrets_round_trip_and_can_be_replaced_and_deleted() {
        let store = InMemorySecretStore::new();
        assert_eq!(
            store.get("k").await.unwrap().map(|s| s.expose().to_owned()),
            None
        );

        store.set("k", &Secret::new("first")).await.unwrap();
        assert_eq!(store.get("k").await.unwrap().unwrap().expose(), "first");

        store.set("k", &Secret::new("second")).await.unwrap();
        assert_eq!(store.get("k").await.unwrap().unwrap().expose(), "second");

        store.delete("k").await.unwrap();
        assert!(store.get("k").await.unwrap().is_none());
        // Deleting something absent is not an error.
        store.delete("k").await.unwrap();
    }

    #[tokio::test]
    async fn an_unavailable_store_fails_instead_of_falling_back() {
        let store = InMemorySecretStore::unavailable();
        assert!(!store.is_available().await);
        let err = store.set("k", &Secret::new("value")).await.unwrap_err();
        assert!(matches!(err, CoreError::SecretStore(_)), "{err:?}");
        // Nothing was written anywhere.
        assert!(store.get("k").await.unwrap().is_none());
    }

    #[test]
    fn the_keyring_store_uses_a_scoped_service_name() {
        assert_eq!(KeyringSecretStore::default().service, "onera");
    }
}
