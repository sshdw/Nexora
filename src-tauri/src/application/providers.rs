//! Provider metadata service: application-layer coordination for configured
//! AI providers (SRS FR-004, FR-014; ROADMAP.md Phase 3 — AI Providers).
//!
//! This service sits in the application layer (ARCHITECTURE.md §5) and
//! orchestrates access to the existing [`ProviderRepository`] and
//! [`CredentialStore`]. It maps repository results to application-facing
//! operations and resolves the presence of provider credentials without ever
//! exposing a secret value.
//!
//! Provider metadata is persisted in the `providers` table (DATABASE.md §7.5)
//! via the repository; this service adds no SQL and never touches the shared
//! connection directly. All persistence is delegated to the repository, and
//! credential presence alone is delegated to [`CredentialStore`]. NO API key,
//! token, or password is ever written to `SQLite` (ARCHITECTURE.md §12;
//! DATABASE.md §14) and no secret value is returned or logged here.
//!
//! "Available" follows DATABASE.md §7.5: a provider's availability is
//! determined by the presence of its configuration (a `providers` row) and its
//! credentials. This service therefore exposes credential-presence and
//! availability helpers so later Phase 3 request execution can select an
//! available provider and detect a missing credential before sending a request.
//! It performs no networking, model discovery, retry, or request execution.

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::providers::credentials::{CredentialError, CredentialStore};
use crate::infrastructure::repository::providers::{Provider, ProviderRepository};

/// Application-layer result shared by provider metadata operations, unifying
/// persistence and keyring failures.
pub(crate) type Result<T> = std::result::Result<T, ProviderError>;

/// Application-layer service coordinating configured AI provider metadata.
///
/// Wraps [`ProviderRepository`] for persistence and composes [`CredentialStore`]
/// for credential-presence checks. It is deliberately focused on orchestration
/// and contains no business logic beyond the availability definition described
/// in the module docs; validation belongs to higher layers.
pub(crate) struct ProviderService<'a> {
    repo: ProviderRepository<'a>,
}

impl<'a> ProviderService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            repo: ProviderRepository::new(db),
        }
    }

    /// Persist a new provider (FR-004).
    ///
    /// Returns the `id` of the newly inserted row. A duplicate internal `name`
    /// or a value rejected by the `providers` CHECK constraints is an error.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Database`] if the insert fails.
    pub(crate) fn create(&self, name: &str, display_name: &str) -> Result<i64> {
        Ok(self.repo.create(name, display_name)?)
    }

    /// Read a provider by database `id`.
    ///
    /// Returns [`Some`] when the provider exists, or [`None`] otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Database`] on a failed query or poisoned
    /// connection.
    pub(crate) fn read_by_id(&self, id: i64) -> Result<Option<Provider>> {
        Ok(self.repo.read(id)?)
    }

    /// Read a provider by its unique internal `name` (DATABASE.md §7.5).
    ///
    /// Returns [`Some`] when the provider exists, or [`None`] otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Database`] on a failed query or poisoned
    /// connection.
    pub(crate) fn read_by_name(&self, name: &str) -> Result<Option<Provider>> {
        Ok(self.repo.read_by_name(name)?)
    }

    /// List every configured provider, ordered by `id` ascending.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Database`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<Provider>> {
        Ok(self.repo.list()?)
    }

    /// Remove a provider by `id`.
    ///
    /// Relies on the existing `messages.provider_id` foreign key
    /// (`ON DELETE SET NULL`) enforced by the schema (DATABASE.md §7.5, §9), so
    /// conversation/message history is preserved; deleting a non-existent `id`
    /// is a no-op. This method never deletes messages.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Database`] if the delete fails.
    pub(crate) fn remove(&self, id: i64) -> Result<()> {
        Ok(self.repo.delete(id)?)
    }

    /// Report whether the provider named `name` has a stored credential
    /// (FR-014); presence only, never the secret value.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Credential`] when the OS keyring cannot be
    /// reached to determine presence.
    pub(crate) fn has_credentials(name: &str) -> Result<bool> {
        Ok(CredentialStore::exists(name)?)
    }

    /// Report whether the provider named `name` is available (FR-004).
    ///
    /// A provider is available when it is configured (a `providers` row exists
    /// under its unique internal `name`, DATABASE.md §7.5) and it has
    /// credentials.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Database`] if the lookup fails, or
    /// [`ProviderError::Credential`] when the OS keyring cannot be reached to
    /// determine credential presence.
    pub(crate) fn is_available(&self, name: &str) -> Result<bool> {
        if self.repo.read_by_name(name)?.is_none() {
            return Ok(false);
        }
        Ok(CredentialStore::exists(name)?)
    }

    /// List the providers that are available (configured and with credentials),
    /// ordered by `id` ascending. This supports selecting an available provider
    /// for subsequent requests.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Database`] if listing fails, or
    /// [`ProviderError::Credential`] when the OS keyring cannot be reached to
    /// determine credential presence for a provider.
    pub(crate) fn available(&self) -> Result<Vec<Provider>> {
        let mut available = Vec::new();
        for provider in self.repo.list()? {
            if CredentialStore::exists(&provider.name)? {
                available.push(provider);
            }
        }
        Ok(available)
    }
}

/// Errors raised by the provider metadata service.
///
/// Unifies persistence ([`DatabaseError`]) and keyring ([`CredentialError`])
/// failures. Both underlying errors carry no secret payload, so formatting a
/// [`ProviderError`] never writes a credential to the logs (ARCHITECTURE.md §9,
/// §11).
#[derive(Debug)]
pub(crate) enum ProviderError {
    /// A persistence failure from the `providers` repository.
    Database(DatabaseError),
    /// A failure while consulting the OS secure keyring for credential
    /// presence.
    Credential(CredentialError),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(err) => write!(f, "{err}"),
            Self::Credential(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ProviderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            Self::Credential(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for ProviderError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

impl From<CredentialError> for ProviderError {
    fn from(err: CredentialError) -> Self {
        Self::Credential(err)
    }
}
