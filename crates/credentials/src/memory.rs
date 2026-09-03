//! In-memory `CredentialStore` for tests and dev runs.
//!
//! Same trait contract, no OS round trip — so crate-level behavior (reference
//! semantics, redaction, not-found handling) is testable everywhere, while the
//! real-keychain integration stays a gated test in `os.rs-tests`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::{CredentialError, CredentialRef, CredentialStore, Secret};

#[derive(Debug, Default)]
pub struct MemoryCredentialStore {
    inner: Mutex<HashMap<(String, String), Vec<u8>>>,
}

impl MemoryCredentialStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Secrets currently held (test inspection only).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().map(|map| map.len()).unwrap_or(0)
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn set(
        &self,
        service: &str,
        account: &str,
        secret: &Secret,
    ) -> Result<CredentialRef, CredentialError> {
        let reference = CredentialRef::new(service, account);
        self.inner
            .lock()
            .map_err(|_| CredentialError::Backend("memory store poisoned".into()))?
            .insert(
                (service.to_string(), account.to_string()),
                secret.as_bytes().to_vec(),
            );
        Ok(reference)
    }

    fn get(&self, reference: &CredentialRef) -> Result<Secret, CredentialError> {
        let map = self
            .inner
            .lock()
            .map_err(|_| CredentialError::Backend("memory store poisoned".into()))?;
        map.get(&(
            reference.service().to_string(),
            reference.account().to_string(),
        ))
        .cloned()
        .map(Secret)
        .ok_or_else(|| CredentialError::NotFound {
            reference: reference.to_string_ref(),
        })
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), CredentialError> {
        let mut map = self
            .inner
            .lock()
            .map_err(|_| CredentialError::Backend("memory store poisoned".into()))?;
        map.remove(&(
            reference.service().to_string(),
            reference.account().to_string(),
        ));
        Ok(())
    }

    fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        let map = self
            .inner
            .lock()
            .map_err(|_| CredentialError::Backend("memory store poisoned".into()))?;
        Ok(map.contains_key(&(
            reference.service().to_string(),
            reference.account().to_string(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_round_trip() {
        let store = MemoryCredentialStore::new();
        let reference = store
            .set(
                "ssh",
                "deploy_key_1",
                &Secret::from_utf8("-----BEGIN PRIVATE KEY-----"),
            )
            .expect("set");
        assert_eq!(reference.to_string_ref(), "keychain://ssh/deploy_key_1");
        assert!(store.has(&reference).expect("has"));
        assert_eq!(
            store.get(&reference).expect("get").as_utf8().expect("utf8"),
            "-----BEGIN PRIVATE KEY-----"
        );
    }

    #[test]
    fn missing_reference_is_not_found() {
        let store = MemoryCredentialStore::new();
        let reference = CredentialRef::parse("keychain://ssh/nope").expect("parse");
        assert!(!store.has(&reference).expect("has"));
        assert!(matches!(
            store.get(&reference),
            Err(CredentialError::NotFound { .. })
        ));
    }

    #[test]
    fn overwrite_replaces_previous_secret() {
        let store = MemoryCredentialStore::new();
        let reference = store
            .set("openai", "default", &Secret::from_utf8("key-1"))
            .expect("set 1");
        store
            .set("openai", "default", &Secret::from_utf8("key-2"))
            .expect("set 2");
        assert_eq!(
            store.get(&reference).expect("get").as_utf8().expect("utf8"),
            "key-2"
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn delete_is_idempotent() {
        let store = MemoryCredentialStore::new();
        let reference = store.set("ssh", "k", &Secret::from_utf8("x")).expect("set");
        store.delete(&reference).expect("delete");
        assert!(matches!(
            store.get(&reference),
            Err(CredentialError::NotFound { .. })
        ));
        // Second delete: already gone, still Ok (idempotent reclaim).
        store.delete(&reference).expect("delete again");
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret::from_utf8("hunter2-super-secret-value");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    }
}
