//! OS-backed `CredentialStore` via `keyring`:
//! macOS Keychain, Windows Credential Manager, Linux Secret Service.
//!
//! Windows Credential Manager caps secret size (~2560 bytes); oversized values
//! surface as [`crate::CredentialError::Backend`] with the backend's message —
//! never with the secret itself.

use crate::{CredentialError, CredentialRef, CredentialStore, Secret};

/// Stateless: every operation addresses the store by service/account.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsCredentialStore;

impl CredentialStore for OsCredentialStore {
    fn set(
        &self,
        service: &str,
        account: &str,
        secret: &Secret,
    ) -> Result<CredentialRef, CredentialError> {
        let reference = CredentialRef::new(service, account);
        let entry = entry(&reference)?;
        entry
            .set_secret(secret.as_bytes())
            .map_err(|error| CredentialError::Backend(error.to_string()))?;
        Ok(reference)
    }

    fn get(&self, reference: &CredentialRef) -> Result<Secret, CredentialError> {
        let entry = entry(reference)?;
        match entry.get_secret() {
            Ok(bytes) => Ok(Secret(bytes)),
            Err(keyring::Error::NoEntry) => Err(CredentialError::NotFound {
                reference: reference.to_string_ref(),
            }),
            Err(other) => Err(CredentialError::Backend(other.to_string())),
        }
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), CredentialError> {
        let entry = entry(reference)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()), // idempotent reclaim
            Err(other) => Err(CredentialError::Backend(other.to_string())),
        }
    }

    fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        match self.get(reference) {
            Ok(_) => Ok(true),
            Err(CredentialError::NotFound { .. }) => Ok(false),
            Err(other) => Err(other),
        }
    }
}

fn entry(reference: &CredentialRef) -> Result<keyring::Entry, CredentialError> {
    keyring::Entry::new(reference.service(), reference.account())
        .map_err(|error| CredentialError::Backend(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real-keychain integration, opt-in: CI never touches a real Keychain /
    /// Credential Manager. Run locally with the variable set to verify a machine.
    fn is_enabled() -> bool {
        std::env::var("YUKINAL_CREDENTIAL_TEST_REAL_KEYRING").is_ok()
    }

    #[test]
    fn os_store_round_trip_when_enabled() {
        if !is_enabled() {
            eprintln!("skipped: set YUKINAL_CREDENTIAL_TEST_REAL_KEYRING=1 to run");
            return;
        }
        let store = OsCredentialStore;
        let reference = CredentialRef::new("yukinal-test", "round-trip");
        store
            .set(
                reference.service(),
                reference.account(),
                &Secret::from_utf8("value"),
            )
            .expect("set");
        assert_eq!(
            store.get(&reference).expect("get").as_utf8().expect("utf8"),
            "value"
        );
        store.delete(&reference).expect("delete");
        assert!(matches!(
            store.get(&reference),
            Err(CredentialError::NotFound { .. })
        ));
    }
}
