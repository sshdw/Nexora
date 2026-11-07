//! Provider credential store: operating-system secure keyring (FR-014).
//!
//! This store persists AI provider credentials (API keys, secrets, tokens) in
//! the operating system secure keyring and nowhere else. Per ARCHITECTURE.md
//! §12 and DATABASE.md §14, provider credentials MUST NEVER be stored in
//! `SQLite`; this is the single place where they live.
//!
//! The store is keyed by a provider's internal `name` (DATABASE.md §7.5). The
//! provider `name` becomes the keyring entry key within a fixed application
//! service namespace, so a provider's credential is recoverable across
//! application restarts by `name`.
//!
//! Operations follow FR-014 exactly: [`CredentialStore::add`],
//! [`CredentialStore::read`], [`CredentialStore::update`],
//! [`CredentialStore::remove`], and [`CredentialStore::exists`] (used to detect
//! missing credentials before sending requests). The store contains **no
//! business logic and no provider validation**; validation belongs to higher
//! application layers, and request execution is a distinct Phase 3 task.
//!
//! Secrets are never hardcoded and never written to the logs (ARCHITECTURE.md
//! §9, §11). Each secret is passed directly between the caller and the keyring
//! without being logged, and [`CredentialError`] deliberately carries no
//! secret payload.
//!
//! The error taxonomy itself lives in `nexora_core::provider` (NEX-ARCH-002:
//! the application execution service consumes it through the
//! [`CredentialSource`] seam without importing this module) and is re-exported
//! here; this module additionally implements the seam so the composition root
//! can install [`CredentialStore`] as the process credential source.
//!
//! Testability (AC-210): the keyring itself sits behind the
//! [`CredentialBackend`] seam. Production wires [`KeyringBackend`] — the OS
//! keyring, one [`keyring::Entry`] per operation, exactly as before the seam —
//! while tests substitute an in-memory backend, so the store's keying, error
//! mapping, and secret-free error guarantee are verified without ever touching
//! the operating-system keyring.

use keyring::Entry;

pub(crate) use nexora_core::provider::CredentialError;

use nexora_core::provider::CredentialSource;

/// Fixed application service namespace for keyring entries.
///
/// The provider internal `name` (DATABASE.md §7.5) is appended as the entry
/// key within this namespace, yielding a stable, collision-free keyring
/// identifier per provider.
const KEYRING_SERVICE: &str = "nexora";

/// Store for AI provider credentials in the operating-system secure keyring.
///
/// Stateless: every operation addresses the keyring through the production
/// [`KeyringBackend`], keyed by the provider's internal `name`. It is
/// intentionally free of business logic and provider validation; higher
/// application layers own those concerns.
pub(crate) struct CredentialStore;

impl CredentialStore {
    /// Store a new credential under `provider` (FR-014).
    ///
    /// `provider` is the provider's internal `name` (DATABASE.md §7.5) used as
    /// the keyring entry key. Because the keyring's write is an upsert, storing
    /// a credential that already exists replaces it; FR-014's distinct
    /// add/update operations are reflected in [`CredentialStore::add`] and
    /// [`CredentialStore::update`].
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::StorageUnavailable`] if the OS keyring is
    /// unavailable, or [`CredentialError::Invalid`] if the credential cannot be
    /// stored as supplied.
    pub(crate) fn add(provider: &str, credential: &str) -> Result<(), CredentialError> {
        add_with(&KeyringBackend, provider, credential)
    }

    /// Read the credential stored under `provider` (FR-014).
    ///
    /// Returns [`Some`] containing the secret when present, or [`None`] when no
    /// credential exists for `provider`.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::StorageUnavailable`] if the OS keyring is
    /// unavailable, or [`CredentialError::Invalid`] if the stored credential
    /// cannot be decoded.
    pub(crate) fn read(provider: &str) -> Result<Option<String>, CredentialError> {
        read_with(&KeyringBackend, provider)
    }
    /// Replace the credential stored under `provider` (FR-014).
    ///
    /// `provider` is the provider's internal `name` (DATABASE.md §7.5). The
    /// keyring write is an upsert, so the new value is stored whether or not a
    /// credential already exists.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::StorageUnavailable`] if the OS keyring is
    /// unavailable, or [`CredentialError::Invalid`] if the credential cannot be
    /// stored as supplied.
    pub(crate) fn update(provider: &str, credential: &str) -> Result<(), CredentialError> {
        update_with(&KeyringBackend, provider, credential)
    }

    /// Remove the credential stored under `provider` (FR-014).
    ///
    /// Removing a provider that has no stored credential is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::StorageUnavailable`] if the OS keyring is
    /// unavailable, or [`CredentialError::Invalid`] if the removal fails for
    /// another reason.
    pub(crate) fn remove(provider: &str) -> Result<(), CredentialError> {
        remove_with(&KeyringBackend, provider)
    }

    /// Report whether `provider` has a stored credential.
    ///
    /// Higher application layers use this to detect missing credentials before
    /// sending a request (FR-014).
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::StorageUnavailable`] when the OS keyring
    /// cannot be reached to determine availability; this is distinct from a
    /// definitively missing credential, which resolves to [`Ok(false)`].
    pub(crate) fn exists(provider: &str) -> Result<bool, CredentialError> {
        exists_with(&KeyringBackend, provider)
    }
}

/// Backend seam for the credential store (AC-210): the keyring primitives,
/// addressed by service namespace and entry key.
///
/// The store's logic (keying, absence semantics, error classification) is
/// generic over this trait, so tests substitute an in-memory backend and never
/// touch the operating-system keyring. Production wires [`KeyringBackend`].
///
/// Each method mirrors one [`keyring::Entry`] operation and returns the raw
/// [`keyring::Error`]: classification into the secret-free
/// [`CredentialError`] taxonomy belongs to the store, not to the backend.
pub(crate) trait CredentialBackend {
    /// Store `credential` under (`service`, `key`). The write is an upsert:
    /// an existing credential is replaced.
    ///
    /// # Errors
    ///
    /// Returns the raw keyring failure, e.g.
    /// [`keyring::Error::NoStorageAccess`] when the store cannot be reached or
    /// [`keyring::Error::TooLong`] when the value exceeds a platform limit.
    fn set_password(&self, service: &str, key: &str, credential: &str) -> keyring::Result<()>;

    /// Read the secret stored under (`service`, `key`).
    ///
    /// # Errors
    ///
    /// Returns [`keyring::Error::NoEntry`] when no secret exists, or the raw
    /// keyring failure otherwise.
    fn get_password(&self, service: &str, key: &str) -> keyring::Result<String>;

    /// Remove the secret stored under (`service`, `key`).
    ///
    /// # Errors
    ///
    /// Returns [`keyring::Error::NoEntry`] when no secret exists, or the raw
    /// keyring failure otherwise.
    fn delete_credential(&self, service: &str, key: &str) -> keyring::Result<()>;
}

/// Production [`CredentialBackend`]: the operating-system secure keyring.
///
/// Opens a fresh [`keyring::Entry`] per operation (the pre-seam behavior) so
/// no entry handle is ever cached. All failures are returned raw; the store
/// classifies them.
pub(crate) struct KeyringBackend;

impl CredentialBackend for KeyringBackend {
    fn set_password(&self, service: &str, key: &str, credential: &str) -> keyring::Result<()> {
        Entry::new(service, key)?.set_password(credential)
    }

    fn get_password(&self, service: &str, key: &str) -> keyring::Result<String> {
        Entry::new(service, key)?.get_password()
    }

    fn delete_credential(&self, service: &str, key: &str) -> keyring::Result<()> {
        Entry::new(service, key)?.delete_credential()
    }
}

/// Store a new credential under `provider` through `backend` (FR-014).
///
/// The keyring write is an upsert, so an existing credential is replaced.
///
/// # Errors
///
/// Maps the raw backend failure through [`classify`]:
/// [`CredentialError::StorageUnavailable`] if the store is unavailable,
/// [`CredentialError::Invalid`] if the credential cannot be stored as
/// supplied.
fn add_with<B: CredentialBackend>(
    backend: &B,
    provider: &str,
    credential: &str,
) -> Result<(), CredentialError> {
    backend
        .set_password(KEYRING_SERVICE, provider, credential)
        .map_err(|err| classify(&err))
}

/// Read the credential stored under `provider` through `backend` (FR-014).
///
/// Returns [`Some`] with the secret when present, [`None`] when absent
/// ([`keyring::Error::NoEntry`]).
///
/// # Errors
///
/// Maps the raw backend failure through [`classify`].
fn read_with<B: CredentialBackend>(
    backend: &B,
    provider: &str,
) -> Result<Option<String>, CredentialError> {
    match backend.get_password(KEYRING_SERVICE, provider) {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(classify(&err)),
    }
}

/// Replace the credential stored under `provider` through `backend` (FR-014).
///
/// The keyring write is an upsert, so the new value is stored whether or not a
/// credential already exists.
///
/// # Errors
///
/// Maps the raw backend failure through [`classify`].
fn update_with<B: CredentialBackend>(
    backend: &B,
    provider: &str,
    credential: &str,
) -> Result<(), CredentialError> {
    backend
        .set_password(KEYRING_SERVICE, provider, credential)
        .map_err(|err| classify(&err))
}

/// Remove the credential stored under `provider` through `backend` (FR-014).
///
/// Removing a provider that has no stored credential ([`keyring::Error::NoEntry`])
/// is a no-op.
///
/// # Errors
///
/// Maps the raw backend failure through [`classify`].
fn remove_with<B: CredentialBackend>(backend: &B, provider: &str) -> Result<(), CredentialError> {
    match backend.delete_credential(KEYRING_SERVICE, provider) {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(classify(&err)),
    }
}

/// Report whether `provider` has a stored credential, through `backend`.
///
/// Absence ([`keyring::Error::NoEntry`]) resolves to [`Ok(false)`]; this is
/// distinct from a store that cannot be reached, which is an error.
///
/// # Errors
///
/// Maps the raw backend failure through [`classify`].
fn exists_with<B: CredentialBackend>(backend: &B, provider: &str) -> Result<bool, CredentialError> {
    match backend.get_password(KEYRING_SERVICE, provider) {
        Ok(_) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(err) => Err(classify(&err)),
    }
}

/// Map a keyring failure to a secret-free [`CredentialError`].
///
/// The absence of an entry ([`keyring::Error::NoEntry`]) is handled by callers
/// before reaching this path and is deliberately not classified here.
fn classify(err: &keyring::Error) -> CredentialError {
    use keyring::Error as KeyringError;
    match err {
        KeyringError::NoStorageAccess(_)
        | KeyringError::PlatformFailure(_)
        | KeyringError::NoDefaultStore
        | KeyringError::NotSupportedByStore(_) => CredentialError::StorageUnavailable,
        // The remaining variants describe unusable, oversized, or malformed
        // data and cannot reach the store.
        _ => CredentialError::Invalid,
    }
}

impl CredentialSource for CredentialStore {
    fn exists(&self, provider: &str) -> Result<bool, CredentialError> {
        Self::exists(provider)
    }

    fn read(&self, provider: &str) -> Result<Option<String>, CredentialError> {
        Self::read(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Obviously-fake fixture credential; never a real secret.
    const TEST_CREDENTIAL: &str = "test-credential-nexora-f6";
    /// Provider internal names (DATABASE.md §7.5) used as keyring entry keys.
    const PROVIDER_A: &str = "openai";
    const PROVIDER_B: &str = "anthropic";

    /// In-memory stand-in for the OS keyring (AC-210).
    ///
    /// Records which (`service`, `key`) pairs each operation addressed, so
    /// tests can assert the store's keying, and can be armed so the next
    /// operation fails with a chosen raw [`keyring::Error`], so tests can
    /// assert the error mapping. Absent entries behave like the real keyring:
    /// reads and deletes of a missing credential return
    /// [`keyring::Error::NoEntry`]. Never touches the OS keyring.
    struct InMemoryBackend {
        entries: Mutex<HashMap<(String, String), String>>,
        failure: Mutex<Option<keyring::Error>>,
    }

    impl InMemoryBackend {
        fn new() -> Self {
            Self {
                entries: Mutex::new(HashMap::new()),
                failure: Mutex::new(None),
            }
        }

        /// Arm the next backend operation to fail with `error`.
        fn fail_next(&self, error: keyring::Error) {
            *self.failure.lock().expect("failure slot") = Some(error);
        }

        /// Snapshot of the stored (`service`, `key`) → secret map.
        fn stored(&self) -> HashMap<(String, String), String> {
            self.entries.lock().expect("entries").clone()
        }

        fn take_failure(&self) -> Option<keyring::Error> {
            self.failure.lock().expect("failure slot").take()
        }
    }

    impl CredentialBackend for InMemoryBackend {
        fn set_password(&self, service: &str, key: &str, credential: &str) -> keyring::Result<()> {
            if let Some(err) = self.take_failure() {
                return Err(err);
            }
            self.entries
                .lock()
                .expect("entries")
                .insert((service.to_owned(), key.to_owned()), credential.to_owned());
            Ok(())
        }

        fn get_password(&self, service: &str, key: &str) -> keyring::Result<String> {
            if let Some(err) = self.take_failure() {
                return Err(err);
            }
            self.entries
                .lock()
                .expect("entries")
                .get(&(service.to_owned(), key.to_owned()))
                .cloned()
                .ok_or(keyring::Error::NoEntry)
        }

        fn delete_credential(&self, service: &str, key: &str) -> keyring::Result<()> {
            if let Some(err) = self.take_failure() {
                return Err(err);
            }
            let mut entries = self.entries.lock().expect("entries");
            if entries
                .remove(&(service.to_owned(), key.to_owned()))
                .is_none()
            {
                return Err(keyring::Error::NoEntry);
            }
            Ok(())
        }
    }

    /// The keyring failure constructors the store classifies as
    /// "storage unavailable", one fresh value per call (`keyring::Error` is
    /// not `Clone`).
    const STORAGE_UNAVAILABLE: [fn() -> keyring::Error; 4] = [
        || keyring::Error::PlatformFailure("platform down".into()),
        || keyring::Error::NoStorageAccess("store locked".into()),
        || keyring::Error::NoDefaultStore,
        || keyring::Error::NotSupportedByStore("operation unsupported".into()),
    ];

    /// The keyring failure constructors the store classifies as
    /// "unusable data" ([`CredentialError::Invalid`]). `Ambiguous` carries an
    /// empty entry list so no real keyring entry is ever constructed.
    const INVALID_DATA: [fn() -> keyring::Error; 6] = [
        || keyring::Error::TooLong("attribute".into(), 16),
        || keyring::Error::Invalid("attribute".into(), "malformed".into()),
        || keyring::Error::BadStoreFormat("store corrupted".into()),
        || keyring::Error::BadEncoding(vec![0xff]),
        || keyring::Error::BadDataFormat(vec![0xff], "undecodable".into()),
        || keyring::Error::Ambiguous(Vec::new()),
    ];

    #[test]
    fn operations_are_keyed_by_service_and_provider_name() {
        let backend = InMemoryBackend::new();

        add_with(&backend, PROVIDER_A, TEST_CREDENTIAL).expect("add first provider");
        add_with(&backend, PROVIDER_B, "second-test-credential").expect("add second provider");

        // Every operation is addressed within the fixed application service
        // namespace (`KEYRING_SERVICE`) and keyed by the provider's internal
        // name, and the two providers never collide.
        let stored = backend.stored();
        assert!(
            stored.contains_key(&(KEYRING_SERVICE.to_owned(), PROVIDER_A.to_owned())),
            "first provider must be keyed under the application service namespace"
        );
        assert!(
            stored.contains_key(&(KEYRING_SERVICE.to_owned(), PROVIDER_B.to_owned())),
            "second provider must be keyed under the application service namespace"
        );
        assert_eq!(stored.len(), 2, "distinct provider names must not collide");

        assert_eq!(
            read_with(&backend, PROVIDER_A).expect("read first"),
            Some(TEST_CREDENTIAL.to_owned())
        );
        assert_eq!(
            read_with(&backend, PROVIDER_B).expect("read second"),
            Some("second-test-credential".to_owned())
        );
        assert!(exists_with(&backend, PROVIDER_A).expect("exists first"));
        assert!(exists_with(&backend, PROVIDER_B).expect("exists second"));
    }

    #[test]
    fn update_replaces_the_stored_credential() {
        let backend = InMemoryBackend::new();
        add_with(&backend, PROVIDER_A, "before").expect("add");

        update_with(&backend, PROVIDER_A, "after").expect("update");

        assert_eq!(
            read_with(&backend, PROVIDER_A).expect("read after update"),
            Some("after".to_owned())
        );
        assert_eq!(
            backend.stored().len(),
            1,
            "the keyring write is an upsert: update must not create a second entry"
        );
    }

    #[test]
    fn read_and_exists_report_missing_credentials() {
        let backend = InMemoryBackend::new();

        assert_eq!(
            read_with(&backend, PROVIDER_A).expect("read of a missing credential"),
            None,
            "a missing credential is None, not an error"
        );
        assert!(
            !exists_with(&backend, PROVIDER_A).expect("exists of a missing credential"),
            "a missing credential resolves to Ok(false), not an error"
        );
    }

    #[test]
    fn remove_deletes_the_credential_and_missing_removal_is_a_no_op() {
        let backend = InMemoryBackend::new();
        add_with(&backend, PROVIDER_A, TEST_CREDENTIAL).expect("add");

        remove_with(&backend, PROVIDER_A).expect("remove");

        assert_eq!(
            read_with(&backend, PROVIDER_A).expect("read after remove"),
            None
        );
        assert!(!exists_with(&backend, PROVIDER_A).expect("exists after remove"));
        assert!(
            !backend
                .stored()
                .contains_key(&(KEYRING_SERVICE.to_owned(), PROVIDER_A.to_owned())),
            "the entry must be gone from the backend"
        );

        // Removing a provider that has no stored credential is a no-op (FR-014):
        // the backend's NoEntry is swallowed by the store.
        remove_with(&backend, PROVIDER_A).expect("remove of a missing credential");
        remove_with(&backend, PROVIDER_B).expect("remove of a never-stored provider");
    }

    #[test]
    fn storage_failures_map_to_storage_unavailable_for_every_operation() {
        for build in STORAGE_UNAVAILABLE {
            let failure = build();

            let backend = InMemoryBackend::new();
            backend.fail_next(build());
            assert!(
                matches!(
                    add_with(&backend, PROVIDER_A, TEST_CREDENTIAL),
                    Err(CredentialError::StorageUnavailable)
                ),
                "add must classify {failure:?} as StorageUnavailable"
            );

            let backend = InMemoryBackend::new();
            backend.fail_next(build());
            assert!(
                matches!(
                    read_with(&backend, PROVIDER_A),
                    Err(CredentialError::StorageUnavailable)
                ),
                "read must classify {failure:?} as StorageUnavailable"
            );

            let backend = InMemoryBackend::new();
            backend.fail_next(build());
            assert!(
                matches!(
                    update_with(&backend, PROVIDER_A, TEST_CREDENTIAL),
                    Err(CredentialError::StorageUnavailable)
                ),
                "update must classify {failure:?} as StorageUnavailable"
            );

            let backend = InMemoryBackend::new();
            backend.fail_next(build());
            assert!(
                matches!(
                    remove_with(&backend, PROVIDER_A),
                    Err(CredentialError::StorageUnavailable)
                ),
                "remove must classify {failure:?} as StorageUnavailable"
            );

            let backend = InMemoryBackend::new();
            backend.fail_next(build());
            assert!(
                matches!(
                    exists_with(&backend, PROVIDER_A),
                    Err(CredentialError::StorageUnavailable)
                ),
                "exists must classify {failure:?} as StorageUnavailable"
            );
        }
    }

    #[test]
    fn unusable_data_failures_map_to_invalid_for_every_backend_operation() {
        for build in INVALID_DATA {
            let failure = build();

            // add and update share the set_password backend operation.
            let backend = InMemoryBackend::new();
            backend.fail_next(build());
            assert!(
                matches!(
                    add_with(&backend, PROVIDER_A, TEST_CREDENTIAL),
                    Err(CredentialError::Invalid)
                ),
                "add must classify {failure:?} as Invalid"
            );

            // read and exists share the get_password backend operation.
            let backend = InMemoryBackend::new();
            backend.fail_next(build());
            assert!(
                matches!(
                    read_with(&backend, PROVIDER_A),
                    Err(CredentialError::Invalid)
                ),
                "read must classify {failure:?} as Invalid"
            );

            // remove is the delete_credential backend operation.
            let backend = InMemoryBackend::new();
            backend.fail_next(build());
            assert!(
                matches!(
                    remove_with(&backend, PROVIDER_A),
                    Err(CredentialError::Invalid)
                ),
                "remove must classify {failure:?} as Invalid"
            );
        }
    }

    #[test]
    fn mapped_errors_carry_no_secret() {
        // The guarantee (ARCHITECTURE.md §9, §11): a formatted CredentialError
        // can never leak a credential, even when the underlying keyring
        // failure's own payload contains one. The seam makes this testable:
        // arm failures whose payloads embed the fixture credential.
        let backend = InMemoryBackend::new();
        backend.fail_next(keyring::Error::PlatformFailure(
            format!("backend failure: {TEST_CREDENTIAL}").into(),
        ));
        let err = add_with(&backend, PROVIDER_A, TEST_CREDENTIAL).expect_err("add fails");
        assert!(matches!(&err, CredentialError::StorageUnavailable));
        assert!(
            !err.to_string().contains(TEST_CREDENTIAL),
            "Display output must not carry the credential"
        );
        assert!(
            !format!("{err:?}").contains(TEST_CREDENTIAL),
            "Debug output must not carry the credential"
        );

        let backend = InMemoryBackend::new();
        backend.fail_next(keyring::Error::TooLong(TEST_CREDENTIAL.to_owned(), 16));
        let err = read_with(&backend, PROVIDER_A).expect_err("read fails");
        assert!(matches!(&err, CredentialError::Invalid));
        assert!(
            !err.to_string().contains(TEST_CREDENTIAL),
            "Display output must not carry the credential"
        );
        assert!(
            !format!("{err:?}").contains(TEST_CREDENTIAL),
            "Debug output must not carry the credential"
        );

        // The store never hands the raw keyring error to the caller: every
        // mapped error is one of the two payload-free taxonomy variants.
        for build in STORAGE_UNAVAILABLE {
            let backend = InMemoryBackend::new();
            backend.fail_next(build());
            let err = update_with(&backend, PROVIDER_A, TEST_CREDENTIAL).expect_err("update fails");
            assert!(matches!(
                &err,
                CredentialError::StorageUnavailable | CredentialError::Invalid
            ));
        }
    }
}
