//! Provider request execution service: the provider-independent execution
//! orchestration of the AI layer (SRS FR-003, FR-004, FR-014; ROADMAP.md
//! Phase 3 — AI Providers; ARCHITECTURE.md §5, §7).
//!
//! The provider-independent *contract* itself ([`ProviderExecutor`],
//! [`AiRequest`]/[`AiResponse`], [`ExecutorError`], [`CancellationToken`])
//! lives in `nexora_core::provider` (NEX-ARCH-002 / AC-101) and is
//! re-exported below so the rest of the crate keeps one import path. This
//! module owns the application-side orchestration on top of that contract:
//! it composes the existing
//! [`ProviderService`](crate::application::providers::ProviderService) for
//! provider metadata and the injected [`CredentialSource`] for credentials,
//! and executes every request through the single provider-agnostic
//! [`ProviderExecutor`] boundary until a classified result is propagated.
//! It invents no provider-specific networking, request format, or API
//! behavior: the concrete provider executors are infrastructure
//! implementations of the nexora-core trait, registered by the composition
//! root (`lib.rs` setup) and reached only through the injected
//! [`ProviderExecutorResolver`].
//!
//! A request is executed only after:
//!   1. the requested provider is resolved through the Provider Metadata
//!      Service (ARCHITECTURE.md §7: provider selection);
//!   2. the provider is verified to be configured and to have stored
//!      credentials (FR-014: missing credentials are detected before a request
//!      is sent; DATABASE.md §7.5 availability);
//!   3. the credential is read from the injected [`CredentialSource`], only
//!      at the moment of execution;
//!   4. the request is delegated to the [`ProviderExecutor`].
//!
//! # Security
//!
//! Per ARCHITECTURE.md §9, §11, §12 and DATABASE.md §14, credential values are
//! NEVER persisted to `SQLite`, written to the logs, or included in any returned
//! metadata or error message. The credential is read directly into this
//! service's execution call and passed only to the executor that performs the
//! network request; it is dropped when the call returns. [`RequestError`] and
//! [`ExecutorError`] deliberately carry no secret payload (ARCHITECTURE.md §10:
//! classified errors).

use std::sync::{Arc, LazyLock, OnceLock};

use crate::infrastructure::database::{Database, DatabaseError};

use super::providers::{ProviderError, ProviderService};
use crate::application::agent::control::CancellationToken;
use crate::application::compat::CompatError;

// The provider-independent execution boundary moved to `nexora_core::provider`
// (NEX-ARCH-002): this facade re-export keeps the crate-wide
// `application::execution::*` import path stable while the dependency arrow
// between application and infrastructure points through the core contract
// only — this module names no concrete provider.
pub(crate) use nexora_core::provider::{
    AiAttachment, AiAttachmentPayload, AiMessage, AiRequest, AiResponse, AiRole, AiToolResult,
    ExecutorError, ModelConfig, ProviderExecutor, TokenUsage, ToolCall, ToolDefinition,
};
use nexora_core::provider::{
    CompatEndpoint, CredentialError, CredentialSource, ProviderExecutorResolver, SharedExecutor,
    OPENAI_COMPAT_NAME,
};

/// Application-layer result shared by request execution operations, unifying
/// orchestration, persistence, and credential failures.
pub(crate) type Result<T> = std::result::Result<T, RequestError>;

// ---------------------------------------------------------------------------
// Injected execution sources
// ---------------------------------------------------------------------------

/// The process-wide provider execution sources, installed once by the
/// composition root (`lib.rs` setup): the concrete provider-executor
/// resolver and the OS keyring credential source.
///
/// Before installation (unit tests never run `lib.rs` setup) the sources
/// behave as an empty registry and a missing-credential store: nothing
/// resolves and every credential check reports "no credential" — the same
/// classified outcomes a production caller sees for an unconfigured
/// provider, never a silent fallback to another provider.
#[derive(Clone)]
pub(crate) struct ExecutionSources {
    executors: Arc<dyn ProviderExecutorResolver>,
    credentials: Arc<dyn CredentialSource>,
}

static INSTALLED_SOURCES: OnceLock<ExecutionSources> = OnceLock::new();

/// The empty sources assumed before [`install_execution_sources`] runs.
static EMPTY_SOURCES: LazyLock<ExecutionSources> = LazyLock::new(|| ExecutionSources {
    executors: Arc::new(nexora_core::provider::ExecutorRegistry::new()),
    credentials: Arc::new(EmptyCredentialSource),
});

/// A credential source modeling "no credential is stored anywhere", used
/// only before the composition root installs the real keyring-backed
/// source. Every check resolves to a definitively missing credential
/// (never a storage failure), so the classified outcome is
/// [`RequestError::MissingCredentials`].
#[derive(Debug)]
struct EmptyCredentialSource;

impl CredentialSource for EmptyCredentialSource {
    fn exists(&self, _provider: &str) -> std::result::Result<bool, CredentialError> {
        Ok(false)
    }

    fn read(&self, _provider: &str) -> std::result::Result<Option<String>, CredentialError> {
        Ok(None)
    }
}

/// Install the process-wide execution sources (composition root only:
/// `lib.rs` setup). Fails when sources are already installed — installation
/// happens exactly once per process.
///
/// # Errors
///
/// Returns an error string when the sources were already installed; startup
/// surfaces it as an unrecoverable configuration error.
pub(crate) fn install_execution_sources(
    executors: Arc<dyn ProviderExecutorResolver>,
    credentials: Arc<dyn CredentialSource>,
) -> std::result::Result<(), String> {
    INSTALLED_SOURCES
        .set(ExecutionSources {
            executors,
            credentials,
        })
        .map_err(|_| "the execution sources are already installed".to_string())
}

/// The active execution sources: the installed ones, or the empty sources
/// before installation (unit tests). Never panics and never builds concrete
/// providers — this module cannot name them.
fn execution_sources() -> ExecutionSources {
    INSTALLED_SOURCES
        .get()
        .cloned()
        .unwrap_or_else(|| EMPTY_SOURCES.clone())
}

/// Resolve a concrete [`ProviderExecutor`] by provider name through the
/// installed registry.
///
/// Shared lookup seam for callers that only need to know whether a provider
/// is executable (agent-run preconditions, health probes) — the resolution
/// itself performs no I/O and reads no credential. Returns [`None`] for an
/// unregistered name; there is deliberately no fallback to another provider.
pub(crate) fn resolve_executor(name: &str) -> Option<SharedExecutor> {
    execution_sources().executors.resolve(name)
}

// ---------------------------------------------------------------------------
// Orchestration service
// ---------------------------------------------------------------------------

/// Application-layer service orchestrating an AI request from provider
/// selection to execution.
///
/// Wraps the existing [`ProviderService`] for provider metadata, composes the
/// injected [`CredentialSource`] for credentials, and resolves the concrete
/// executor through the injected [`ProviderExecutorResolver`]. It is
/// deliberately focused on orchestration and contains no provider-specific
/// behavior: once a provider is resolved, its credential is available, and a
/// registered executor is found, the request is delegated to that
/// provider-independent [`ProviderExecutor`].
pub(crate) struct RequestExecutionService<'a> {
    provider: ProviderService<'a>,
    sources: ExecutionSources,
    db: &'a Database,
}

impl<'a> RequestExecutionService<'a> {
    /// Create a service over the shared application [`Database`], resolving
    /// executors and credentials through the process-wide execution sources
    /// installed by the composition root.
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            provider: ProviderService::new(db),
            sources: execution_sources(),
            db,
        }
    }

    /// Create a service with explicit sources (tests): the service reads
    /// credentials and resolves executors only through what is injected
    /// here — construction touches no keyring and builds no providers.
    #[cfg(test)]
    pub(crate) fn with_sources(db: &'a Database, sources: ExecutionSources) -> Self {
        Self {
            provider: ProviderService::new(db),
            sources,
            db,
        }
    }

    /// Resolve the credential for `provider` (Task 5.1 factoring): the
    /// provider-metadata lookup plus the FR-014 credential checks that
    /// [`Self::execute`] performs, exposed so the agent-run bridge can
    /// resolve the credential before spawning the run thread. The value is
    /// never persisted, logged, or placed in any returned metadata.
    ///
    /// # Errors
    ///
    /// Same classification as [`Self::execute`] steps 1–3.
    pub(crate) fn resolve_credential(&self, provider: &str) -> Result<String> {
        // 1. Resolve the requested provider through the Provider Metadata
        //    Service (ARCHITECTURE.md §7: provider selection).
        let provider = self
            .provider
            .read_by_name(provider)
            .map_err(RequestError::from)?
            .ok_or_else(|| RequestError::UnknownProvider {
                name: provider.to_string(),
            })?;

        // 2. The provider must be available: configured and credentialed
        //    (DATABASE.md §7.5; FR-014 detects a missing credential before a
        //    request is sent). An unreachable keyring is not a definitively
        //    missing credential, so it is classified separately as an
        //    unavailable provider.
        match self.sources.credentials.exists(&provider.name) {
            Err(CredentialError::StorageUnavailable) => {
                return Err(RequestError::ProviderUnavailable {
                    name: provider.name,
                });
            }
            Err(err) => return Err(RequestError::Credential(err)),
            Ok(false) => {
                return Err(RequestError::MissingCredentials {
                    name: provider.name,
                })
            }
            Ok(true) => {}
        }

        // 3. Obtain the credential only now, immediately before execution, and
        //    only through the injected CredentialSource (FR-014). The value is
        //    never persisted, logged, or placed in any returned metadata.
        match self.sources.credentials.read(&provider.name) {
            Ok(Some(secret)) => Ok(secret),
            Ok(None) => Err(RequestError::MissingCredentials {
                name: provider.name,
            }),
            Err(CredentialError::StorageUnavailable) => Err(RequestError::ProviderUnavailable {
                name: provider.name,
            }),
            Err(err) => Err(RequestError::Credential(err)),
        }
    }

    /// Execute `request`.
    ///
    /// Resolves the requested provider through the Provider Metadata Service,
    /// verifies it is configured and has stored credentials, reads the
    /// credential from the injected [`CredentialSource`] only for the duration
    /// of the call, and delegates execution to the [`ProviderExecutor`].
    ///
    /// # Errors
    ///
    /// Returns [`RequestError::UnknownProvider`] when no provider with the
    /// request's `provider` name is configured; [`RequestError::MissingCredentials`]
    /// when the provider exists but has no stored credential;
    /// [`RequestError::ProviderUnavailable`] when the secure keyring cannot be
    /// reached to determine or obtain the credential;
    /// [`RequestError::ExecutorUnavailable`] when the provider is configured but
    /// has no registered executor; [`RequestError::Execution`]
    /// when the provider fails to fulfil the request; or
    /// [`RequestError::Database`] when the provider lookup fails.
    pub(crate) fn execute(&self, request: &AiRequest) -> Result<AiResponse> {
        // 1–3. Provider row, availability, and credential (shared with the
        //     Task 5.1 agent-run bridge through `resolve_credential`).
        let credential = self.resolve_credential(&request.provider)?;

        // 4. Resolve the concrete executor for this provider through the
        //    injected registry — except for the user-configured
        //    OpenAI-compatible endpoint, whose base URL, organization,
        //    headers, and tool support live in settings rather than in a
        //    static entry. The config is validated before any request is
        //    built; the credential resolved above still comes only from the
        //    keyring-backed source.
        if request.provider == OPENAI_COMPAT_NAME {
            let config = crate::application::compat::CompatEndpointService::new(self.db)
                .read_config()
                .map_err(RequestError::from)?;
            config
                .validate()
                .map_err(|err| RequestError::InvalidCompatEndpoint {
                    reason: err.to_string(),
                })?;
            let executor = self.sources.executors.compat_executor(&CompatEndpoint {
                endpoint: config.normalized_endpoint(),
                organization: config.organization.clone(),
                headers: config.headers.clone(),
                supports_tools: config.supports_tools,
            });
            let idle_token = CancellationToken::new();
            return executor
                .execute(request, &credential, &idle_token)
                .map_err(|err| RequestError::Execution {
                    name: request.provider.clone(),
                    message: err.to_string(),
                });
        }
        let executor = self
            .sources
            .executors
            .resolve(&request.provider)
            .ok_or_else(|| RequestError::ExecutorUnavailable {
                name: request.provider.clone(),
            })?;

        // 5. Delegate to the provider-independent boundary. The credential is
        //    moved only into this call and dropped when it returns. Plain
        //    chat has no run to cancel, so it executes under a fresh token
        //    that never fires; cancellation is an agent-run concern.
        let idle_token = CancellationToken::new();
        executor
            .execute(request, &credential, &idle_token)
            .map_err(|err| RequestError::Execution {
                name: request.provider.clone(),
                message: err.to_string(),
            })
    }
}

/// Classified errors raised by request execution.
///
/// Unifies orchestration failures and persistence failures. No variant carries
/// a credential or other secret value, so formatting a [`RequestError`] never
/// writes a secret to the logs (ARCHITECTURE.md §9, §10, §11).
#[derive(Debug)]
pub(crate) enum RequestError {
    /// No provider with the requested `name` is configured.
    UnknownProvider {
        /// The requested provider internal name.
        name: String,
    },
    /// The provider is configured but its credential could not be obtained
    /// because the OS secure keyring is unreachable.
    ProviderUnavailable {
        /// The provider internal name.
        name: String,
    },
    /// The provider is configured but has no stored credential (FR-014).
    MissingCredentials {
        /// The provider internal name.
        name: String,
    },
    /// The provider is configured but no executor is registered for its
    /// internal name, so the request cannot be executed.
    ExecutorUnavailable {
        /// The provider internal name.
        name: String,
    },
    /// The user-configured OpenAI-compatible endpoint is missing or invalid;
    /// `reason` is the secret-free validation category (never a URL,
    /// header value, or credential).
    InvalidCompatEndpoint {
        /// The secret-free validation failure category.
        reason: String,
    },
    /// The provider failed to fulfil the request; `message` carries the
    /// classified error text (never a credential or payload).
    Execution {
        /// The provider internal name.
        name: String,
        /// The classified provider error text.
        message: String,
    },
    /// A credential-store failure other than an unreachable keyring.
    Credential(CredentialError),
    /// A provider metadata lookup failure.
    Database(DatabaseError),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider { name } => {
                write!(f, "the AI provider '{name}' is not configured")
            }
            Self::ProviderUnavailable { name } => write!(
                f,
                "the AI provider '{name}' is unavailable: its credentials could not \
                 be reached in the OS secure keyring"
            ),
            Self::MissingCredentials { name } => write!(
                f,
                "the AI provider '{name}' has no stored credentials (FR-014)"
            ),
            Self::ExecutorUnavailable { name } => {
                write!(f, "the AI provider '{name}' has no registered executor")
            }
            Self::InvalidCompatEndpoint { reason } => {
                write!(
                    f,
                    "the OpenAI-compatible endpoint is not configured correctly: {reason}"
                )
            }
            Self::Execution { name, message } => {
                write!(f, "the AI provider '{name}' failed: {message}")
            }
            Self::Credential(err) => write!(f, "{err}"),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnknownProvider { .. }
            | Self::ProviderUnavailable { .. }
            | Self::MissingCredentials { .. }
            | Self::ExecutorUnavailable { .. }
            | Self::InvalidCompatEndpoint { .. }
            | Self::Execution { .. } => None,
            Self::Credential(err) => Some(err),
            Self::Database(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for RequestError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

impl From<ProviderError> for RequestError {
    fn from(err: ProviderError) -> Self {
        match err {
            ProviderError::Database(err) => Self::Database(err),
            ProviderError::Credential(err) => Self::Credential(err),
        }
    }
}

impl From<CompatError> for RequestError {
    fn from(err: CompatError) -> Self {
        match err {
            CompatError::Database(err) => Self::Database(err),
            // Stored settings are user endpoint metadata, never secrets; the
            // reason is already a secret-free category.
            CompatError::InvalidStoredData { reason } => Self::InvalidCompatEndpoint { reason },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexora_core::provider::ExecutorRegistry;
    use std::sync::Mutex;

    /// A credential source with a fixed verdict per call.
    struct StubCredentials {
        exists: bool,
        secret: Option<String>,
    }

    impl StubCredentials {
        fn with_secret(secret: &str) -> Self {
            Self {
                exists: true,
                secret: Some(secret.to_string()),
            }
        }

        fn without_secret() -> Self {
            Self {
                exists: false,
                secret: None,
            }
        }
    }

    impl CredentialSource for StubCredentials {
        fn exists(&self, _provider: &str) -> std::result::Result<bool, CredentialError> {
            Ok(self.exists)
        }

        fn read(&self, _provider: &str) -> std::result::Result<Option<String>, CredentialError> {
            Ok(self.secret.clone())
        }
    }

    /// An executor stub that records the credential it was handed and
    /// returns a fixed classified outcome.
    struct StubExecutor {
        response: Option<AiResponse>,
        error_factory: Option<fn() -> ExecutorError>,
        seen_credentials: Mutex<Vec<String>>,
    }

    impl StubExecutor {
        fn responding(content: &str) -> Self {
            Self {
                response: Some(AiResponse {
                    content: content.to_string(),
                    model: "m".to_string(),
                    tool_calls: Vec::new(),
                    usage: None,
                }),
                error_factory: None,
                seen_credentials: Mutex::new(Vec::new()),
            }
        }

        fn failing(error_factory: fn() -> ExecutorError) -> Self {
            Self {
                response: None,
                error_factory: Some(error_factory),
                seen_credentials: Mutex::new(Vec::new()),
            }
        }

        fn credentials_seen(&self) -> Vec<String> {
            self.seen_credentials
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }
    }

    impl ProviderExecutor for StubExecutor {
        fn execute(
            &self,
            _request: &AiRequest,
            credential: &str,
            _token: &CancellationToken,
        ) -> std::result::Result<AiResponse, ExecutorError> {
            if let Ok(mut seen) = self.seen_credentials.lock() {
                seen.push(credential.to_string());
            }
            if let Some(response) = &self.response {
                return Ok(response.clone());
            }
            Err((self.error_factory.expect("stub has an error factory"))())
        }
    }

    fn sources_for(executors: ExecutorRegistry, credentials: StubCredentials) -> ExecutionSources {
        ExecutionSources {
            executors: Arc::new(executors),
            credentials: Arc::new(credentials),
        }
    }

    fn chat_request(provider: &str) -> AiRequest {
        AiRequest {
            provider: provider.to_string(),
            model: "m".to_string(),
            messages: Arc::new(vec![AiMessage {
                role: AiRole::User,
                content: "hello".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }]),
            tools: Arc::new(Vec::new()),
            model_config: None,
            request_timeout: None,
        }
    }

    fn database_with_provider(name: &str) -> Database {
        let db = crate::infrastructure::database::in_memory_database();
        db.lock()
            .expect("database lock")
            .execute(
                "INSERT INTO providers (name, display_name) VALUES (?1, ?1)",
                [name],
            )
            .expect("seed provider row");
        db
    }

    #[test]
    fn request_service_round_trips_through_injected_sources() {
        let executor = Arc::new(StubExecutor::responding("stub answer"));
        let registered: SharedExecutor = Arc::<StubExecutor>::clone(&executor);
        let credentials = StubCredentials::with_secret("sentinel-credential");
        let sources = sources_for(
            ExecutorRegistry::new().with_executor("openai", registered),
            credentials,
        );
        let db = database_with_provider("openai");
        let service = RequestExecutionService::with_sources(&db, sources);

        let response = service
            .execute(&chat_request("openai"))
            .expect("injected executor answers");
        assert_eq!(response.content, "stub answer");
        // The credential read inside the keyring-backed source reached the
        // executor and nothing else; the stub never saw a different value.
        assert_eq!(executor.credentials_seen(), vec!["sentinel-credential"]);
    }

    #[test]
    fn request_service_unknown_provider_is_classified_without_execution() {
        let executor = Arc::new(StubExecutor::responding("never"));
        let registered: SharedExecutor = Arc::<StubExecutor>::clone(&executor);
        let credentials = StubCredentials::with_secret("sentinel-credential");
        let sources = sources_for(
            ExecutorRegistry::new().with_executor("openai", registered),
            credentials,
        );
        let db = database_with_provider("openai");
        let service = RequestExecutionService::with_sources(&db, sources);

        let err = service
            .execute(&chat_request("ghost"))
            .expect_err("unconfigured provider is classified");
        assert!(matches!(err, RequestError::UnknownProvider { .. }));
        assert_eq!(err.to_string(), "the AI provider 'ghost' is not configured");
        assert!(
            executor.credentials_seen().is_empty(),
            "no credential may reach an executor when the provider is unknown"
        );
    }

    #[test]
    fn request_service_missing_credential_is_classified_before_execution() {
        let executor = Arc::new(StubExecutor::responding("never"));
        let registered: SharedExecutor = Arc::<StubExecutor>::clone(&executor);
        let sources = sources_for(
            ExecutorRegistry::new().with_executor("openai", registered),
            StubCredentials::without_secret(),
        );
        let db = database_with_provider("openai");
        let service = RequestExecutionService::with_sources(&db, sources);

        let err = service
            .execute(&chat_request("openai"))
            .expect_err("missing credential is classified (FR-014)");
        assert!(matches!(err, RequestError::MissingCredentials { .. }));
        assert!(
            executor.credentials_seen().is_empty(),
            "no executor may run without a credential"
        );
    }

    #[test]
    fn request_service_unregistered_provider_never_falls_back() {
        let sources = sources_for(
            ExecutorRegistry::new(),
            StubCredentials::with_secret("sentinel-credential"),
        );
        let db = database_with_provider("openai");
        let service = RequestExecutionService::with_sources(&db, sources);

        // The provider is configured and credentialed, but no executor is
        // registered for it: classified as unavailable, never resolved to
        // another provider's executor.
        let err = service
            .execute(&chat_request("openai"))
            .expect_err("empty registry resolves nothing");
        assert!(matches!(err, RequestError::ExecutorUnavailable { .. }));
    }

    #[test]
    fn request_service_executor_failure_is_classified_secret_free() {
        let executor = Arc::new(StubExecutor::failing(|| ExecutorError::Authentication));
        let sources = sources_for(
            ExecutorRegistry::new().with_executor("openai", executor),
            StubCredentials::with_secret("sentinel-credential"),
        );
        let db = database_with_provider("openai");
        let service = RequestExecutionService::with_sources(&db, sources);

        let err = service
            .execute(&chat_request("openai"))
            .expect_err("executor failure is classified");
        assert!(matches!(err, RequestError::Execution { .. }));
        let message = err.to_string();
        assert!(message.contains("openai"));
        assert!(
            !message.contains("sentinel-credential"),
            "the credential must never surface in a classified error: {message:?}"
        );
    }

    #[test]
    fn resolve_credential_reads_only_through_the_injected_source() {
        let credentials = StubCredentials::with_secret("sentinel-credential");
        let sources = sources_for(ExecutorRegistry::new(), credentials);
        let db = database_with_provider("openai");
        let service = RequestExecutionService::with_sources(&db, sources);

        let credential = service
            .resolve_credential("openai")
            .expect("credential resolves");
        assert_eq!(credential, "sentinel-credential");
    }

    #[test]
    fn resolve_executor_returns_none_before_installation() {
        // Process-global invariant: no unit test installs the execution
        // sources (that happens only in `lib.rs` setup), so the lookup falls
        // back to the empty registry — it resolves nothing and never
        // silently fabricates a provider.
        assert!(resolve_executor("openai").is_none());
        assert!(resolve_executor("ghost").is_none());
    }

    #[test]
    fn request_execution_display_includes_classified_message() {
        let err = RequestError::Execution {
            name: "openai".into(),
            message: "boom".into(),
        };
        assert_eq!(err.to_string(), "the AI provider 'openai' failed: boom");
    }

    #[test]
    fn invalid_compat_endpoint_error_is_secret_free() {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        let err = RequestError::InvalidCompatEndpoint {
            reason: "the base URL must use the http or https scheme".to_string(),
        };
        let message = err.to_string();
        assert!(
            message.contains("OpenAI-compatible"),
            "compat misconfiguration must name the endpoint: {message:?}"
        );
        for sentinel in SECRET_SENTINELS {
            assert!(
                !message.to_lowercase().contains(sentinel),
                "compat error must stay secret-free, found {sentinel:?}"
            );
        }
    }
}
