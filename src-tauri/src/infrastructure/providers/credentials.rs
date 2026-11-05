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
/// Stateless: every operation addresses the keyring directly, keyed by the
/// provider's internal `name`. It is intentionally free of business logic and
/// provider validation; higher application layers own those concerns.
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
        entry(provider)?
            .set_password(credential)
            .map_err(|err| classify(&err))
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
        match entry(provider)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(classify(&err)),
        }
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
        entry(provider)?
            .set_password(credential)
            .map_err(|err| classify(&err))
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
        match entry(provider)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(classify(&err)),
        }
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
        match entry(provider)?.get_password() {
            Ok(_) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(err) => Err(classify(&err)),
        }
    }
}

/// Resolve the keyring entry for `provider` within the application namespace.
fn entry(provider: &str) -> Result<Entry, CredentialError> {
    Entry::new(KEYRING_SERVICE, provider).map_err(|err| classify(&err))
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
