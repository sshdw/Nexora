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

use serde::Serialize;

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::providers::credentials::{CredentialError, CredentialStore};
use crate::infrastructure::providers::openai::COMPAT_NAME;
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

    /// Probe the local health of the provider named `name` (WS-A.5).
    ///
    /// The probe is deliberately lightweight and local-only: it composes the
    /// existing [`Self::has_credentials`] presence check with the existing
    /// provider-row lookup (the two signals behind [`Self::is_available`])
    /// plus a local executor-registry lookup. It performs no network I/O, so
    /// `Healthy` means "locally ready to serve" rather than "remotely
    /// reachable".
    ///
    /// Classification (see [`classify_health`] for the pure decision table):
    /// - `Unknown`: `name` is not a build-supported provider.
    /// - `Unreachable`: known, but unconfigured or missing its credential.
    /// - `Degraded`: configured and credentialed, but no executor is
    ///   registered for it. The user-configured OpenAI-compatible endpoint
    ///   (`openai_compat`) is exempt: its executor is assembled per request
    ///   from settings, so static-registry absence is not a degradation.
    /// - `Healthy`: configured, credentialed, and executable.
    ///
    /// The returned [`ProviderHealth`] carries presence booleans and a
    /// `last_checked` Unix-seconds timestamp only — never a secret value.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Database`] if the configuration lookup fails,
    /// or [`ProviderError::Credential`] when the OS keyring cannot be reached
    /// to determine credential presence.
    pub(crate) fn health(&self, name: &str) -> Result<ProviderHealth> {
        let known = crate::infrastructure::providers::supported_providers()
            .iter()
            .any(|entry| entry.name == name);
        let configured = self.read_by_name(name)?.is_some();
        let credentialed = Self::has_credentials(name)?;
        let executable = name == COMPAT_NAME
            || super::execution::ExecutorRegistry::shared()
                .resolve(name)
                .is_some();
        Ok(ProviderHealth {
            provider: name.to_string(),
            status: classify_health(HealthSignals {
                known,
                configured,
                credentialed,
                executable,
            }),
            has_configuration: configured,
            has_credential: credentialed,
            last_checked: unix_now_seconds(),
        })
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

/// Local health verdict for one provider (WS-A.5).
///
/// Local-only by design: the probe performs no network I/O, so these states
/// describe local readiness (configuration, credential presence, registered
/// executor), not remote reachability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ProviderHealthStatus {
    /// Configured, credentialed, and executable.
    Healthy,
    /// Configured and credentialed, but no executor is registered, so
    /// requests cannot be served yet.
    Degraded,
    /// Cannot serve requests: unconfigured or missing its credential.
    Unreachable,
    /// Not a build-supported provider; no assessment is possible.
    Unknown,
}

/// Local health snapshot for one provider (WS-A.5).
///
/// Carries metadata only: presence booleans and a `last_checked` Unix-seconds
/// timestamp. Never carries a credential value (ARCHITECTURE.md §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderHealth {
    /// Internal provider name that was probed.
    pub provider: String,
    /// Local readiness verdict.
    pub status: ProviderHealthStatus,
    /// Whether a `providers` row exists for this name.
    pub has_configuration: bool,
    /// Whether the OS keyring holds a credential for this name.
    pub has_credential: bool,
    /// When the probe ran, in Unix seconds. `0` means the system clock was
    /// unavailable at probe time — treat it as "unknown", never as the epoch.
    pub last_checked: u64,
}

/// Local signals feeding the health decision table.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HealthSignals {
    /// `name` is listed by the build's `supported_providers()` registry.
    known: bool,
    /// A `providers` row exists for `name`.
    configured: bool,
    /// The OS keyring holds a credential for `name`.
    credentialed: bool,
    /// An executor can serve `name` (registered, or the dynamically-built
    /// user-configured endpoint).
    executable: bool,
}

/// Decide a [`ProviderHealthStatus`] from local signals (WS-A.5).
///
/// Pure decision table over the four signals, with no I/O, so the full
/// taxonomy is unit-testable without a database or keyring:
/// - unknown names are `Unknown` regardless of the other signals;
/// - a missing configuration or a missing credential is `Unreachable` (the
///   missing-credential path fails before any request is sent, FR-014);
/// - a configured, credentialed provider with no executor is `Degraded`;
/// - otherwise the provider is locally `Healthy`.
fn classify_health(signals: HealthSignals) -> ProviderHealthStatus {
    if !signals.known {
        return ProviderHealthStatus::Unknown;
    }
    if !signals.configured || !signals.credentialed {
        return ProviderHealthStatus::Unreachable;
    }
    if !signals.executable {
        return ProviderHealthStatus::Degraded;
    }
    ProviderHealthStatus::Healthy
}

/// Current wall-clock time in Unix seconds for `last_checked` timestamps.
///
/// Returns `0` when the system clock is unavailable (before the Unix epoch);
/// callers surface that as "unknown", never as the epoch itself.
fn unix_now_seconds() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "test-only shorthand for the four-signal decision table"
    )]
    fn signals(
        known: bool,
        configured: bool,
        credentialed: bool,
        executable: bool,
    ) -> HealthSignals {
        HealthSignals {
            known,
            configured,
            credentialed,
            executable,
        }
    }

    #[test]
    fn fully_ready_provider_is_healthy() {
        assert_eq!(
            classify_health(signals(true, true, true, true)),
            ProviderHealthStatus::Healthy
        );
    }

    #[test]
    fn unknown_provider_name_is_unknown_regardless_of_other_signals() {
        for (configured, credentialed, executable) in [
            (false, false, false),
            (true, true, true),
            (true, false, true),
        ] {
            assert_eq!(
                classify_health(signals(false, configured, credentialed, executable)),
                ProviderHealthStatus::Unknown,
                "unknown names stay Unknown for ({configured}, {credentialed}, {executable})"
            );
        }
    }

    #[test]
    fn missing_credential_is_unreachable() {
        // FR-014: a configured provider without a stored credential fails
        // before any request is sent, so health reports Unreachable.
        assert_eq!(
            classify_health(signals(true, true, false, true)),
            ProviderHealthStatus::Unreachable
        );
    }

    #[test]
    fn missing_configuration_is_unreachable() {
        assert_eq!(
            classify_health(signals(true, false, true, true)),
            ProviderHealthStatus::Unreachable
        );
        assert_eq!(
            classify_health(signals(true, false, false, false)),
            ProviderHealthStatus::Unreachable
        );
    }

    #[test]
    fn credentialed_but_not_executable_is_degraded() {
        assert_eq!(
            classify_health(signals(true, true, true, false)),
            ProviderHealthStatus::Degraded
        );
    }

    #[test]
    fn health_status_serializes_lowercase_for_ipc() {
        let cases = [
            (ProviderHealthStatus::Healthy, "\"healthy\""),
            (ProviderHealthStatus::Degraded, "\"degraded\""),
            (ProviderHealthStatus::Unreachable, "\"unreachable\""),
            (ProviderHealthStatus::Unknown, "\"unknown\""),
        ];
        for (status, expected) in cases {
            assert_eq!(
                serde_json::to_string(&status).expect("status serializes"),
                expected
            );
        }
    }

    #[test]
    fn provider_errors_stay_secret_free() {
        const SENTINELS: [&str; 3] = ["sk-", "secret", "credential value"];
        let errors = [
            ProviderError::Credential(CredentialError::StorageUnavailable),
            ProviderError::Credential(CredentialError::Invalid),
        ];
        for err in errors {
            let rendered = format!("{err}");
            for sentinel in SENTINELS {
                assert!(
                    !rendered.to_lowercase().contains(sentinel),
                    "provider error must stay secret-free, found {sentinel:?} in {rendered:?}"
                );
            }
        }
    }
}
