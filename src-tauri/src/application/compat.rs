//! User-configured OpenAI-compatible endpoint service: application-layer
//! coordination for a custom base URL, model, organization, and extra headers
//! (provider-runtime extension).
//!
//! This service sits in the application layer (ARCHITECTURE.md §5) and
//! orchestrates the existing [`SettingsService`] for persistence. It adds no
//! SQL and never touches the shared connection directly. The endpoint API key
//! is never handled here: it belongs exclusively to the OS keyring under
//! [`COMPAT_NAME`] (ARCHITECTURE.md §12; DATABASE.md §14), and only its
//! *presence* (resolved by the caller from [`CredentialStore`]) enters the
//! [`CompatStatus`] projection.
//!
//! Validation lives in [`CompatConfig::validate`]; this service only
//! persists a validated config, reads it back, and projects
//! presence booleans for the UI. Stored values are endpoint metadata, never
//! secrets — and error reasons never echo stored content (header values may
//! carry alternate credentials).

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::providers::openai::{CompatConfig, COMPAT_NAME};

use super::settings::SettingsService;

/// Application-layer result for compatible-endpoint operations.
pub(crate) type Result<T> = std::result::Result<T, CompatError>;

/// Settings keys backing the endpoint configuration. All live under the
/// `openai_compat.` namespace so they cannot collide with other settings.
const KEY_BASE_URL: &str = "openai_compat.base_url";
const KEY_MODEL: &str = "openai_compat.model";
const KEY_ORGANIZATION: &str = "openai_compat.organization";
const KEY_HEADERS_JSON: &str = "openai_compat.headers_json";
const KEY_SUPPORTS_TOOLS: &str = "openai_compat.supports_tools";

/// Upper bound on the stored headers document (bytes): validation caps the
/// parsed entries, this cap bounds the read against a hostile local value.
const MAX_STORED_HEADERS_JSON: usize = 64 * 1_024;

/// UI-facing status of the user-configured endpoint: presence booleans plus
/// readiness, carrying no secret material.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CompatStatus {
    /// A base URL value is configured (non-blank).
    pub has_base_url: bool,
    /// The configured base URL passes validation.
    pub base_url_valid: bool,
    /// A model identifier is configured (non-blank).
    pub has_model: bool,
    /// An organization identifier is configured.
    pub has_organization: bool,
    /// Number of configured extra headers.
    pub header_count: usize,
    /// The endpoint has a stored keyring credential (caller-resolved).
    pub has_credential: bool,
    /// The endpoint can serve a request: valid config plus credential.
    pub ready: bool,
}

/// Application-layer service coordinating the user-configured endpoint.
pub(crate) struct CompatEndpointService<'a> {
    settings: SettingsService<'a>,
}

impl<'a> CompatEndpointService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            settings: SettingsService::new(db),
        }
    }

    /// Provider internal name this service configures.
    pub(crate) fn provider_name() -> &'static str {
        COMPAT_NAME
    }

    /// Read the stored endpoint configuration.
    ///
    /// Absent keys resolve to the empty [`CompatConfig::default`]; a corrupt
    /// headers document is [`CompatError::InvalidStoredData`] with a
    /// category-only reason (stored values are never echoed).
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Database`] on a failed query or
    /// [`CompatError::InvalidStoredData`] when the stored headers document
    /// cannot be parsed.
    pub(crate) fn read_config(&self) -> Result<CompatConfig> {
        let base_url = self.settings.read(KEY_BASE_URL)?.unwrap_or_default();
        let model = self.settings.read(KEY_MODEL)?.unwrap_or_default();
        let organization = self.settings.read(KEY_ORGANIZATION)?;
        let headers = match self.settings.read(KEY_HEADERS_JSON)? {
            None => Vec::new(),
            Some(document) => parse_stored_headers(&document)?,
        };
        let supports_tools = match self.settings.read(KEY_SUPPORTS_TOOLS)? {
            None => true,
            Some(value) => value.trim() != "false",
        };
        Ok(CompatConfig {
            base_url,
            model,
            organization,
            headers,
            supports_tools,
        })
    }

    /// Persist `config` after validating it.
    ///
    /// The config is rejected unchanged when [`CompatConfig::validate`]
    /// fails; failures are secret-free categories. The API key is never part
    /// of the config and is never written here.
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::InvalidStoredData`] when the config is invalid,
    /// or [`CompatError::Database`] when the write fails.
    pub(crate) fn write_config(&self, config: &CompatConfig) -> Result<()> {
        config
            .validate()
            .map_err(|err| CompatError::InvalidStoredData {
                reason: err.to_string(),
            })?;
        self.settings.write(KEY_BASE_URL, Some(&config.base_url))?;
        self.settings.write(KEY_MODEL, Some(&config.model))?;
        self.settings
            .write(KEY_ORGANIZATION, config.organization.as_deref())?;
        let document =
            serde_json::to_string(&config.headers).map_err(|_| CompatError::InvalidStoredData {
                reason: "the extra headers could not be serialized".to_string(),
            })?;
        self.settings.write(KEY_HEADERS_JSON, Some(&document))?;
        self.settings.write(
            KEY_SUPPORTS_TOOLS,
            Some(if config.supports_tools {
                "true"
            } else {
                "false"
            }),
        )?;
        Ok(())
    }

    /// Project the UI-facing [`CompatStatus`]: field presence from the stored
    /// config plus the caller-resolved keyring credential presence.
    ///
    /// # Errors
    ///
    /// Same as [`Self::read_config`].
    pub(crate) fn status(&self, has_credential: bool) -> Result<CompatStatus> {
        let config = self.read_config()?;
        let fields = config.field_presence();
        Ok(CompatStatus {
            has_base_url: fields.has_base_url,
            base_url_valid: fields.base_url_valid,
            has_model: fields.has_model,
            has_organization: fields.has_organization,
            header_count: fields.header_count,
            has_credential,
            ready: config.validate().is_ok() && has_credential,
        })
    }
}

/// Parse the stored headers document: a JSON array of `[name, value]` pairs.
///
/// Shape or content failures are category-only — the stored document is never
/// echoed (header values may carry alternate credentials).
fn parse_stored_headers(document: &str) -> Result<Vec<(String, String)>> {
    if document.len() > MAX_STORED_HEADERS_JSON {
        return Err(CompatError::InvalidStoredData {
            reason: "the stored extra headers exceed the size limit".to_string(),
        });
    }
    serde_json::from_str(document).map_err(|_| CompatError::InvalidStoredData {
        reason: "the stored extra headers are not valid JSON".to_string(),
    })
}

/// Errors raised by the compatible-endpoint service.
///
/// Never carries stored content or secret material: reasons are fixed
/// categories, so formatting a [`CompatError`] cannot leak a header value or
/// key into the logs (ARCHITECTURE.md §9, §11).
#[derive(Debug)]
pub(crate) enum CompatError {
    /// A persistence failure from the settings repository.
    Database(DatabaseError),
    /// Stored (or supplied) endpoint data failed validation; `reason` is a
    /// secret-free category, never the offending content.
    InvalidStoredData {
        /// Secret-free failure category.
        reason: String,
    },
}

impl std::fmt::Display for CompatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(err) => write!(f, "{err}"),
            Self::InvalidStoredData { reason } => {
                write!(
                    f,
                    "the OpenAI-compatible endpoint configuration is invalid: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for CompatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            Self::InvalidStoredData { .. } => None,
        }
    }
}

impl From<DatabaseError> for CompatError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn service(db: &Database) -> CompatEndpointService<'_> {
        CompatEndpointService::new(db)
    }

    /// Open an in-memory database carrying the production `app_settings`
    /// table shape (DATABASE.md §7.6) so the settings-backed service reads
    /// and writes end to end.
    fn test_db() -> Database {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE app_settings (
                key TEXT PRIMARY KEY CHECK (length(key) > 0 AND length(key) <= 200),
                value TEXT CHECK (length(value) <= 10000)
            );",
        )
        .expect("create test schema");
        Database::new(conn)
    }

    fn valid_config() -> CompatConfig {
        CompatConfig {
            base_url: "https://proxy.example.com/v1/chat/completions".to_string(),
            model: "custom-model".to_string(),
            organization: Some("org-1".to_string()),
            headers: vec![("X-Custom".to_string(), "value".to_string())],
            supports_tools: true,
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let db = test_db();
        let service = service(&db);
        service.write_config(&valid_config()).expect("write valid");
        let back = service.read_config().expect("read back");
        assert_eq!(back, valid_config());
    }

    #[test]
    fn write_rejects_invalid_config_unchanged() {
        let db = test_db();
        let service = service(&db);
        let mut bad = valid_config();
        bad.base_url = "ftp://proxy.example.com/v1".to_string();
        let err = service.write_config(&bad).expect_err("invalid must fail");
        assert!(matches!(err, CompatError::InvalidStoredData { .. }));
        // Nothing was persisted: the read is still the empty default.
        let back = service.read_config().expect("read back");
        assert_eq!(back, CompatConfig::default());
    }

    #[test]
    fn status_reports_presence_and_readiness() {
        let db = test_db();
        let service = service(&db);
        // Empty store, no credential: nothing present, not ready.
        let status = service.status(false).expect("status");
        assert!(!status.has_base_url);
        assert!(!status.base_url_valid);
        assert!(!status.has_model);
        assert!(!status.ready);

        service.write_config(&valid_config()).expect("write valid");
        // Configured but no credential: present, still not ready.
        let status = service.status(false).expect("status");
        assert!(status.has_base_url);
        assert!(status.base_url_valid);
        assert!(status.has_model);
        assert!(status.has_organization);
        assert_eq!(status.header_count, 1);
        assert!(!status.has_credential);
        assert!(!status.ready);
        // Credential present: ready.
        let status = service.status(true).expect("status");
        assert!(status.has_credential);
        assert!(status.ready);
    }

    #[test]
    fn corrupt_headers_document_fails_without_echo() {
        let db = test_db();
        let service = service(&db);
        service
            .settings
            .write(KEY_HEADERS_JSON, Some("[[\"X-K\",\"sk-live-sentinel-1\"]"))
            .expect("plant corrupt document");
        let err = service.read_config().expect_err("corrupt must fail");
        let message = err.to_string();
        assert!(
            !message.contains("sk-live-sentinel-1"),
            "stored content must never be echoed: {message:?}"
        );
        assert!(matches!(err, CompatError::InvalidStoredData { .. }));
    }

    #[test]
    fn provider_name_is_the_compat_identity() {
        assert_eq!(CompatEndpointService::provider_name(), "openai_compat");
    }
}
