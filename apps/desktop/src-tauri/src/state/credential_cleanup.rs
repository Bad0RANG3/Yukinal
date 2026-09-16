use yukinal_credentials::{CredentialRef, CredentialStore};
use yukinal_database::Database;

pub(crate) fn reclaim(
    database: &Database,
    credentials: &dyn CredentialStore,
    reference: &CredentialRef,
) -> Result<(), String> {
    let raw = reference.to_string_ref();
    match credentials.delete(reference) {
        Ok(()) => database.credential_cleanup().remove(&raw).map_err(|error| {
            format!("credential was removed, but its queue entry was not: {error}")
        }),
        Err(error) => {
            database
                .credential_cleanup()
                .enqueue(&raw, &error.to_string(), &yukinal_core::sidecar::iso8601_now())
                .map_err(|queue_error| {
                    format!(
                        "credential reclaim failed ({error}) and could not be queued ({queue_error})"
                    )
                })?;
            Err(format!(
                "credential reclaim failed and was queued for retry: {error}"
            ))
        }
    }
}

pub(crate) fn reconcile(database: &Database, credentials: &dyn CredentialStore) -> Vec<String> {
    let pending = match database.credential_cleanup().list() {
        Ok(pending) => pending,
        Err(error) => return vec![format!("could not read credential cleanup queue: {error}")],
    };
    let mut failures = Vec::new();
    for item in pending {
        let reference = match CredentialRef::parse(&item.reference) {
            Ok(reference) => reference,
            Err(error) => {
                let message = format!(
                    "queued credential reference `{}` is invalid: {error}",
                    item.reference
                );
                if let Err(queue_error) = database.credential_cleanup().enqueue(
                    &item.reference,
                    &message,
                    &yukinal_core::sidecar::iso8601_now(),
                ) {
                    failures.push(format!(
                        "{message}; updating the queue failed: {queue_error}"
                    ));
                } else {
                    failures.push(message);
                }
                continue;
            }
        };
        match credentials.delete(&reference) {
            Ok(()) => {
                if let Err(error) = database.credential_cleanup().remove(&item.reference) {
                    failures.push(format!(
                        "reclaimed `{}` but could not remove its queue entry: {error}",
                        item.reference
                    ));
                }
            }
            Err(error) => {
                let message = format!("could not reclaim queued `{}`: {error}", item.reference);
                if let Err(queue_error) = database.credential_cleanup().enqueue(
                    &item.reference,
                    &message,
                    &yukinal_core::sidecar::iso8601_now(),
                ) {
                    failures.push(format!(
                        "{message}; updating the queue failed: {queue_error}"
                    ));
                } else {
                    failures.push(message);
                }
            }
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use yukinal_credentials::memory::MemoryCredentialStore;
    use yukinal_credentials::{CredentialError, Secret};

    fn temp_db(name: &str) -> (PathBuf, Database) {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "yukinal-cleanup-{}-{}-{}.sqlite",
            std::process::id(),
            name,
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", path.display())));
        }
        let database = Database::open(&path).expect("open temp database");
        (path, database)
    }

    #[test]
    fn startup_reconciliation_retries_and_removes_a_queued_secret() {
        let (path, database) = temp_db("success");
        let credentials = MemoryCredentialStore::new();
        let reference = credentials
            .set("ssh", "queued", &Secret::from_utf8("secret"))
            .expect("secret");
        database
            .credential_cleanup()
            .enqueue(
                &reference.to_string_ref(),
                "previous failure",
                "2026-01-01T00:00:00.000Z",
            )
            .expect("queue");

        assert!(reconcile(&database, &credentials).is_empty());
        assert!(database
            .credential_cleanup()
            .list()
            .expect("list")
            .is_empty());
        assert!(!credentials.has(&reference).expect("lookup"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn failed_reclaim_is_persisted_for_the_next_startup() {
        let (path, database) = temp_db("failure");
        let credentials = FailingCredentialStore;
        let reference = CredentialRef::new("ssh", "cannot-delete");

        let error = reclaim(&database, &credentials, &reference).expect_err("failure");
        assert!(error.contains("queued"));
        let queued = database.credential_cleanup().list().expect("list");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].reference, reference.to_string_ref());
        assert_eq!(queued[0].attempts, 1);

        let _ = std::fs::remove_file(&path);
    }

    #[derive(Debug)]
    struct FailingCredentialStore;

    impl CredentialStore for FailingCredentialStore {
        fn set(
            &self,
            _service: &str,
            _account: &str,
            _secret: &Secret,
        ) -> std::result::Result<CredentialRef, CredentialError> {
            unreachable!("not used")
        }

        fn get(&self, _reference: &CredentialRef) -> std::result::Result<Secret, CredentialError> {
            unreachable!("not used")
        }

        fn delete(&self, reference: &CredentialRef) -> std::result::Result<(), CredentialError> {
            Err(CredentialError::Backend(format!(
                "cannot delete {}",
                reference
            )))
        }

        fn has(&self, _reference: &CredentialRef) -> std::result::Result<bool, CredentialError> {
            unreachable!("not used")
        }
    }
}
