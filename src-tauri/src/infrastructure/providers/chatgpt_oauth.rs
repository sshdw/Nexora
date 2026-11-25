//! Opt-in ChatGPT Plus/Pro OAuth 2.0 + PKCE (`S256`) flow and OS keyring-only
//! token persistence (`S11-11a`).
//!
//! # Security invariants
//! - Strictly opt-in: never invoked automatically on startup or without explicit
//!   user action.
//! - Access, refresh, and ID tokens live EXCLUSIVELY in the OS secure keyring
//!   (`keyring` crate via [`CredentialBackend`]) — NEVER in `SQLite`, IPC
//!   payloads, error messages, or logs.
//! - PKCE uses `S256` (`SHA-256` + `base64url` no-pad) with 256-bit CSPRNG
//!   verifiers (`Uuid::new_v4` x 2).
//! - CSRF `state` is 256-bit CSPRNG and verified in constant time.
//! - Loopback callback binds strictly to `127.0.0.1` with a hard timeout,
//!   bounded request read, and static XSS-free HTML responses (`no-store`).
//! - OAuth HTTP requests disable redirects so authorization codes
//!   and refresh tokens are never forwarded to unexpected hosts.

#![allow(clippy::doc_markdown)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use nexora_core::provider::CancellationToken;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};
use uuid::Uuid;

use super::credentials::{CredentialBackend, KeyringBackend};

/// Dedicated OS keyring service namespace for provider OAuth tokens so entries
/// can never collide with API keys under `"nexora"` or MCP tokens under
/// `"nexora-mcp"`.
pub(crate) const CHATGPT_OAUTH_KEYRING_SERVICE: &str = "nexora-oauth";

/// OS keyring entry key for the opt-in ChatGPT Plus/Pro OAuth grant.
pub(crate) const CHATGPT_OAUTH_KEYRING_KEY: &str = "chatgpt-plus-pro";

/// Default OpenAI / ChatGPT OAuth issuer.
pub(crate) const DEFAULT_ISSUER: &str = "https://auth.openai.com";

/// Default public OAuth client ID for ChatGPT Plus/Pro PKCE login.
const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
const HOST_ID_KEY: &str = "chatgpt-plan-host";
const PLAN_RESOURCE: &str = "https://api.openai.com/v1";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";

/// Default OAuth scopes requested during opt-in ChatGPT sign-in.
const DEFAULT_SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

/// Preferred loopback callback ports tried before falling back to an ephemeral
/// loopback port (`127.0.0.1:0`).
pub(crate) const DEFAULT_CALLBACK_PORTS: [u16; 2] = [1455, 1457];

/// Default wall-clock deadline waiting for the browser OAuth callback.
pub(crate) const DEFAULT_OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_mins(2);

/// Default HTTP timeout for token exchange and refresh requests.
pub(crate) const DEFAULT_OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// Refresh access tokens when fewer than this many seconds remain before expiry.
const REFRESH_SKEW_SECS: i64 = 60;

/// Allowed clock-skew leeway (in seconds) when validating `id_token` `iat` claims.
const ID_TOKEN_CLOCK_SKEW_SECS: i64 = 120;

/// Maximum bytes read from a loopback HTTP callback request.
const MAX_CALLBACK_REQUEST_BYTES: usize = 8192;

/// Maximum bytes read from a token endpoint response.
const MAX_TOKEN_RESPONSE_BYTES: usize = 128 * 1024;

/// Maximum UTF-8 characters stored in a single OS keyring entry so every chunk
/// stays safely below Windows Credential Manager's 2560-byte (`1280` UTF-16
/// char) `CRED_MAX_CREDENTIAL_BLOB_SIZE` limit (`keyring::Error::TooLong`).
const MAX_KEYRING_CHUNK_CHARS: usize = 1000;

/// Maximum number of keyring chunks allowed for one OAuth token bundle.
/// 64 chunks * 1000 UTF-16 chars comfortably accommodates large OpenAI JWT
/// payloads (access_token + id_token + refresh_token + account model catalog)
/// while keeping every Windows Credential Manager entry below the 1280-char limit.
const MAX_KEYRING_CHUNKS: usize = 64;

// Serialize the keyring transaction, refresh, and disconnect. The epoch also
// invalidates any sign-in that is waiting outside the lock for its browser.
static SESSION_LOCK: Mutex<()> = Mutex::new(());
// Bind the process lease and OS keyring scope to the same canonical app-data
// directory so processes with distinct database roots never contend for the
// same keyring slots, host identity, or disconnect markers.
static DESKTOP_LEASE: Mutex<Option<DesktopLease>> = Mutex::new(None);
static BROWSER_SIGN_IN: Mutex<()> = Mutex::new(());
static ACTIVE_SIGN_IN_CANCEL: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);
static DISCONNECT_REQUIRED: AtomicBool = AtomicBool::new(false);
static SESSION_EPOCH: AtomicU64 = AtomicU64::new(0);
static ACTIVE_CALLS: Mutex<Vec<(Uuid, CancellationToken)>> = Mutex::new(Vec::new());
const DISCONNECT_PENDING: &str = "disconnect-pending";

/// Deterministic keyring namespace bound to a relocatable app-data directory.
///
/// Production always acquires a [`KeyringScope`] alongside the directory lease
/// via [`ensure_desktop_lease`]; unit tests without a filesystem directory may
/// use [`KeyringScope::unscoped`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyringScope {
    id: Option<String>,
}

impl KeyringScope {
    #[must_use]
    pub(crate) const fn unscoped() -> Self {
        Self { id: None }
    }

    /// Derive a stable, collision-resistant keyring scope from a canonical
    /// app-data directory path.
    pub(crate) fn for_directory(directory: &Path) -> Result<Self, ChatGptOAuthError> {
        let canonical =
            std::fs::canonicalize(directory).map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
        #[cfg(windows)]
        let normalized = canonical
            .to_string_lossy()
            .to_ascii_lowercase()
            .into_bytes();
        #[cfg(not(windows))]
        let normalized = canonical.as_os_str().as_encoded_bytes().to_vec();
        let digest = Sha256::digest(&normalized);
        let mut id = String::with_capacity(32);
        for byte in &digest[..16] {
            use std::fmt::Write as _;
            let _ = write!(&mut id, "{byte:02x}");
        }
        Ok(Self { id: Some(id) })
    }

    #[must_use]
    fn root_key(&self) -> String {
        match &self.id {
            Some(id) => format!("{CHATGPT_OAUTH_KEYRING_KEY}.{id}"),
            None => CHATGPT_OAUTH_KEYRING_KEY.to_owned(),
        }
    }

    #[must_use]
    fn chunk_key(&self, slot: u8, index: usize) -> String {
        format!("{}.s{slot}.{index}", self.root_key())
    }

    #[must_use]
    fn host_id_key(&self) -> String {
        match &self.id {
            Some(id) => format!("{HOST_ID_KEY}.{id}"),
            None => HOST_ID_KEY.to_owned(),
        }
    }
}

struct DesktopLease {
    _lock: std::fs::File,
    scope: KeyringScope,
}

pub(crate) fn ensure_desktop_lease() -> Result<KeyringScope, ChatGptOAuthError> {
    let mut lease = DESKTOP_LEASE
        .lock()
        .map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
    if let Some(existing) = lease.as_ref() {
        return Ok(existing.scope.clone());
    }
    let database = nexora_core::storage::app_data::database_path()
        .ok_or(ChatGptOAuthError::KeyringUnavailable)?;
    let directory = database
        .parent()
        .ok_or(ChatGptOAuthError::KeyringUnavailable)?;
    let lock = acquire_desktop_lease(directory)?;
    let scope = KeyringScope::for_directory(directory)?;
    *lease = Some(DesktopLease {
        _lock: lock,
        scope: scope.clone(),
    });
    Ok(scope)
}

fn acquire_desktop_lease(directory: &Path) -> Result<std::fs::File, ChatGptOAuthError> {
    let failure = ChatGptOAuthError::KeyringUnavailable;
    let directory =
        Dir::open_ambient_dir(directory, cap_std::ambient_authority()).map_err(|_| failure)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .follow(FollowSymlinks::No);
    let file = directory
        .open_with("chatgpt-oauth.lock", &options)
        .map_err(|_| failure)?
        .into_std();
    if !file.metadata().map_err(|_| failure)?.is_file() {
        return Err(failure);
    }
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(ChatGptOAuthError::OtherInstance),
        Err(std::fs::TryLockError::Error(_)) => Err(failure),
    }
}

fn session_lock() -> Result<std::sync::MutexGuard<'static, ()>, ChatGptOAuthError> {
    SESSION_LOCK
        .lock()
        .map_err(|_| ChatGptOAuthError::KeyringUnavailable)
}

fn require_no_pending_disconnect() -> Result<(), ChatGptOAuthError> {
    if DISCONNECT_REQUIRED.load(Ordering::SeqCst) {
        return Err(ChatGptOAuthError::KeyringUnavailable);
    }
    Ok(())
}

struct SignInCancelGuard {
    flag: Arc<AtomicBool>,
}

impl SignInCancelGuard {
    fn register() -> Result<Self, ChatGptOAuthError> {
        let flag = Arc::new(AtomicBool::new(false));
        let mut slot = ACTIVE_SIGN_IN_CANCEL
            .lock()
            .map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
        *slot = Some(Arc::clone(&flag));
        Ok(Self { flag })
    }

    fn signal(&self) -> &AtomicBool {
        &self.flag
    }
}

impl Drop for SignInCancelGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = ACTIVE_SIGN_IN_CANCEL.lock() {
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.flag))
            {
                *slot = None;
            }
        }
    }
}

fn cancel_active_sign_in() {
    if let Ok(slot) = ACTIVE_SIGN_IN_CANCEL.lock() {
        if let Some(flag) = slot.as_ref() {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

fn invalidate_session() -> Result<(), ChatGptOAuthError> {
    SESSION_EPOCH.fetch_add(1, Ordering::SeqCst);
    cancel_active_sign_in();
    let mut active = ACTIVE_CALLS
        .lock()
        .map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
    for (_, token) in active.drain(..) {
        token.cancel();
    }
    Ok(())
}

pub(crate) struct ActivePlanCall {
    id: Uuid,
    pub(crate) access_token: String,
}

impl Drop for ActivePlanCall {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE_CALLS.lock() {
            active.retain(|(id, _)| *id != self.id);
        }
    }
}

/// Classified, secret-free errors raised by the ChatGPT OAuth flow.
///
/// No variant ever carries tokens, authorization codes, URLs, or raw HTTP
/// response bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatGptOAuthError {
    /// OS secure keyring is unreachable or refused the operation.
    KeyringUnavailable,
    /// Configured OAuth issuer or endpoint URL is invalid or insecure.
    InvalidEndpoint,
    /// Failed to bind the `127.0.0.1` loopback callback socket.
    LoopbackBindFailed,
    /// Failed to launch the user's system browser for sign-in.
    BrowserOpenFailed,
    /// The browser sign-in did not complete before the callback deadline.
    CallbackTimeout,
    /// Returned `state` parameter did not match the expected CSRF token.
    StateMismatch,
    /// The user or authorization server denied or cancelled sign-in.
    AuthorizationDenied,
    /// The callback arrived without a valid authorization code.
    MissingCode,
    /// The authorization code exchange failed or returned an invalid payload.
    TokenExchangeFailed,
    /// The OAuth callback did not include a valid issued client identifier.
    CallbackClientIdInvalid,
    /// The OAuth token endpoint rejected the request with an HTTP error status.
    TokenEndpointRejected { status: u16 },
    /// Verifying the OAuth ID token signature or claims failed.
    IdTokenVerificationFailed,
    /// Fetching or parsing the issuer's JWKS signing keys failed.
    JwksFetchFailed,
    /// Refreshing the stored OAuth token failed or no refresh token is stored.
    TokenRefreshFailed,
    /// Refreshing the stored OAuth token failed transiently (network/5xx/rate limit);
    /// the stored grant remains intact for retry.
    TokenRefreshTransient,
    /// The stored OAuth grant was terminally rejected or revoked and requires
    /// interactive reauthorization (the verified client registration is preserved).
    ReauthorizationRequired,
    /// Token endpoint response exceeded the safety size limit.
    ResponseTooLarge,
    /// No ChatGPT OAuth session is currently stored in the keyring.
    NotConnected,
    /// The OAuth sign-in was cancelled before completion.
    Cancelled,
    /// Local credentials were cleared, but the issuer did not confirm revocation.
    RevocationUnconfirmed,
    /// A separate Nexora process currently owns the OAuth session.
    OtherInstance,
}

impl std::fmt::Display for ChatGptOAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeyringUnavailable => {
                write!(f, "OS secure keyring is unavailable for ChatGPT OAuth")
            }
            Self::InvalidEndpoint => write!(f, "ChatGPT OAuth endpoint URL is invalid"),
            Self::LoopbackBindFailed => {
                write!(f, "failed to start local 127.0.0.1 OAuth callback listener")
            }
            Self::BrowserOpenFailed => {
                write!(f, "could not open system browser for ChatGPT sign-in")
            }
            Self::CallbackTimeout => {
                write!(f, "ChatGPT OAuth sign-in timed out waiting for callback")
            }
            Self::StateMismatch => {
                write!(f, "ChatGPT OAuth CSRF state verification failed")
            }
            Self::AuthorizationDenied => {
                write!(f, "ChatGPT OAuth authorization was denied or cancelled")
            }
            Self::MissingCode => {
                write!(
                    f,
                    "ChatGPT OAuth callback did not include an authorization code"
                )
            }
            Self::TokenExchangeFailed => {
                write!(f, "ChatGPT OAuth token exchange failed")
            }
            Self::CallbackClientIdInvalid => {
                write!(
                    f,
                    "ChatGPT OAuth callback did not include a valid client ID"
                )
            }
            Self::TokenEndpointRejected { status } => {
                write!(
                    f,
                    "ChatGPT OAuth token endpoint rejected the request (HTTP {status})"
                )
            }
            Self::IdTokenVerificationFailed => {
                write!(f, "ChatGPT OAuth ID token verification failed")
            }
            Self::JwksFetchFailed => {
                write!(f, "failed to fetch ChatGPT OAuth signing keys (JWKS)")
            }
            Self::TokenRefreshFailed => {
                write!(
                    f,
                    "ChatGPT OAuth token refresh failed; please sign in again"
                )
            }
            Self::TokenRefreshTransient => {
                write!(
                    f,
                    "ChatGPT OAuth token refresh failed temporarily; please try again"
                )
            }
            Self::ReauthorizationRequired => {
                write!(
                    f,
                    "ChatGPT OAuth authorization expired or was revoked; please sign in again"
                )
            }
            Self::ResponseTooLarge => {
                write!(f, "ChatGPT OAuth token response exceeded the safety limit")
            }
            Self::NotConnected => write!(f, "ChatGPT OAuth is not connected"),
            Self::Cancelled => write!(f, "ChatGPT OAuth sign-in was cancelled or is already in progress"),
            Self::RevocationUnconfirmed => write!(
                f,
                "ChatGPT was disconnected locally. Revoke the connection in ChatGPT Settings to end the remote session."
            ),
            Self::OtherInstance => write!(f, "Another Nexora instance is using ChatGPT sign-in"),
        }
    }
}

impl std::error::Error for ChatGptOAuthError {}

/// Configuration for the ChatGPT OAuth 2.0 + PKCE client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatGptOAuthConfig {
    pub issuer: String,
    pub client_id: String,
    pub scopes: String,
    pub callback_ports: Vec<u16>,
}

impl Default for ChatGptOAuthConfig {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.to_string(),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: DEFAULT_CALLBACK_PORTS.to_vec(),
        }
    }
}

impl ChatGptOAuthConfig {
    /// Validate and build the authorization endpoint URL.
    fn authorize_endpoint(&self) -> Result<Url, ChatGptOAuthError> {
        let base = format!(
            "{}/api/accounts/authorize",
            self.issuer.trim_end_matches('/')
        );
        validate_oauth_endpoint_url(&base)
    }

    /// Validate and build the token endpoint URL.
    fn token_endpoint(&self) -> Result<Url, ChatGptOAuthError> {
        let base = format!(
            "{}/api/accounts/oauth/token",
            self.issuer.trim_end_matches('/')
        );
        validate_oauth_endpoint_url(&base)
    }
}

/// Validate an OAuth endpoint URL: requires `https://` (or explicit loopback
/// `http://127.0.0.1` / `http://localhost` for local tests), forbids embedded
/// credentials, fragments, and control characters.
fn validate_oauth_endpoint_url(raw: &str) -> Result<Url, ChatGptOAuthError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > 2048
        || trimmed.bytes().any(|b| b.is_ascii_control() || b == b' ')
    {
        return Err(ChatGptOAuthError::InvalidEndpoint);
    }
    let parsed = Url::parse(trimmed).map_err(|_| ChatGptOAuthError::InvalidEndpoint)?;
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        return Err(ChatGptOAuthError::InvalidEndpoint);
    }
    let Some(host) = parsed.host() else {
        return Err(ChatGptOAuthError::InvalidEndpoint);
    };
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" => {
            let is_loopback = match host {
                Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
                Host::Ipv4(ip) => ip.is_loopback() && ip.octets() == [127, 0, 0, 1],
                Host::Ipv6(ip) => ip.is_loopback(),
            };
            if is_loopback {
                Ok(parsed)
            } else {
                Err(ChatGptOAuthError::InvalidEndpoint)
            }
        }
        _ => Err(ChatGptOAuthError::InvalidEndpoint),
    }
}

/// Secret-free ChatGPT OAuth status returned across IPC and UI layers.
/// Never carries access tokens, refresh tokens, or ID tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ChatGptOAuthStatus {
    pub connected: bool,
    pub has_refresh_token: bool,
    pub expires_at: Option<i64>,
    pub connected_at: Option<i64>,
    pub auth_mode: &'static str,
    pub problem: Option<ChatGptOAuthProblem>,
    pub models: Vec<PlanModel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChatGptOAuthProblem {
    CleanupRequired,
    OtherInstance,
    ReauthorizationRequired,
}

impl ChatGptOAuthStatus {
    #[must_use]
    pub(crate) const fn disconnected() -> Self {
        Self {
            connected: false,
            has_refresh_token: false,
            expires_at: None,
            connected_at: None,
            auth_mode: "none",
            problem: None,
            models: Vec::new(),
        }
    }
}

/// Secret OAuth token bundle persisted EXCLUSIVELY inside the OS keyring.
/// Never serialized to `SQLite`, IPC payloads, or logs.
/// Only a verified, issuer-bound ID token can identify a saved registration.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredChatGptOAuthTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default)]
    pub connected_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<PlanIdentity>,
    #[serde(default)]
    pub reauthorization_required: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PlanIdentity {
    client_id: String,
    subject: String,
    id_token: String,
    #[serde(default)]
    models: Vec<PlanModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlanModel {
    slug: String,
    display_name: String,
}

fn has_plan_scope(scope: Option<&str>) -> bool {
    scope.is_none_or(|scope| scope.split_ascii_whitespace().any(|s| s == PLAN_SCOPE))
}

fn is_issued_client(client_id: &str) -> bool {
    let suffix = client_id
        .strip_prefix("oaiapp_")
        .or_else(|| client_id.strip_prefix("app_"));
    suffix.is_some_and(|suffix| {
        !suffix.is_empty()
            && client_id.len() <= 256
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

impl std::fmt::Debug for StoredChatGptOAuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredChatGptOAuthTokens")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .field("connected_at", &self.connected_at)
            .finish_non_exhaustive()
    }
}

impl StoredChatGptOAuthTokens {
    #[must_use]
    pub(crate) fn reauthorization_required(mut identity: PlanIdentity) -> Self {
        identity.models.clear();
        Self {
            access_token: String::new(),
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            connected_at: 0,
            identity: Some(identity),
            reauthorization_required: true,
        }
    }

    #[must_use]
    pub(crate) fn status(&self) -> ChatGptOAuthStatus {
        let has_verified_identity = self
            .identity
            .as_ref()
            .is_some_and(|identity| is_issued_client(&identity.client_id));
        let has_refresh = self
            .refresh_token
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        let expired_without_refresh =
            !has_refresh && self.expires_at.is_some_and(|exp| now_unix_secs() >= exp);
        let reauth_required =
            has_verified_identity && (self.reauthorization_required || expired_without_refresh);
        let connected = has_verified_identity
            && !reauth_required
            && has_plan_scope(self.scope.as_deref())
            && !self.access_token.trim().is_empty();
        let problem = if connected {
            None
        } else if reauth_required {
            Some(ChatGptOAuthProblem::ReauthorizationRequired)
        } else {
            Some(ChatGptOAuthProblem::CleanupRequired)
        };
        ChatGptOAuthStatus {
            connected,
            has_refresh_token: connected && has_refresh,
            expires_at: if connected { self.expires_at } else { None },
            connected_at: if connected && self.connected_at > 0 {
                Some(self.connected_at)
            } else {
                None
            },
            auth_mode: if connected { "oauth" } else { "none" },
            problem,
            models: self
                .identity
                .as_ref()
                .filter(|_| connected)
                .map_or_else(Vec::new, |identity| identity.models.clone()),
        }
    }

    #[must_use]
    pub(crate) fn needs_refresh(&self, now_secs: i64) -> bool {
        self.expires_at
            .is_some_and(|exp| now_secs.saturating_add(REFRESH_SKEW_SECS) >= exp)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct KeyringChunkManifest {
    slot: u8,
    chunks: usize,
    #[serde(default)]
    cleanup_pending: bool,
}

#[cfg(test)]
fn chunk_entry_key(slot: u8, index: usize) -> String {
    KeyringScope::unscoped().chunk_key(slot, index)
}

fn split_into_keyring_chunks(payload: &str) -> Result<Vec<String>, ChatGptOAuthError> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut char_count = 0usize;
    for ch in payload.chars() {
        if char_count + ch.len_utf16() > MAX_KEYRING_CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
            char_count = 0;
        }
        current.push(ch);
        char_count += ch.len_utf16();
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    if chunks.len() > MAX_KEYRING_CHUNKS {
        return Err(ChatGptOAuthError::ResponseTooLarge);
    }
    Ok(chunks)
}

/// OS keyring store for opt-in ChatGPT Plus/Pro OAuth tokens.
///
/// Splits serialized token bundles across `<= 1000`-character keyring chunks
/// using a two-slot write-new-then-delete-stale commit protocol so Windows
/// Credential Manager's 2560-byte (`1280` UTF-16 char) blob limit is never
/// exceeded and an existing refresh token is never lost if a save fails.
pub(crate) struct ChatGptOAuthStore;

impl ChatGptOAuthStore {
    #[cfg(test)]
    pub(crate) fn save_with(
        backend: &dyn CredentialBackend,
        tokens: &StoredChatGptOAuthTokens,
    ) -> Result<(), ChatGptOAuthError> {
        Self::save_with_scope(backend, &KeyringScope::unscoped(), tokens)
    }

    pub(crate) fn save_with_scope(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
        tokens: &StoredChatGptOAuthTokens,
    ) -> Result<(), ChatGptOAuthError> {
        let payload =
            serde_json::to_string(tokens).map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
        let chunks = split_into_keyring_chunks(&payload)?;

        let previous = Self::read_root(backend, scope)?;
        let mut previous_manifest = previous
            .as_deref()
            .map(Self::parse_root)
            .transpose()?
            .flatten();
        // If a prior commit crashed or failed while deleting its stale slot,
        // finish deleting that stale slot first without touching the active slot.
        if let Some(prev) = previous_manifest.as_mut() {
            Self::finish_pending_cleanup(backend, scope, prev)?;
        }
        let next_slot = previous_manifest.map_or(0, |manifest| 1 - manifest.slot);
        Self::delete_slot(backend, scope, next_slot, MAX_KEYRING_CHUNKS)?;

        for (index, chunk) in chunks.iter().enumerate() {
            let key = scope.chunk_key(next_slot, index);
            if backend
                .set_password(CHATGPT_OAUTH_KEYRING_SERVICE, &key, chunk)
                .is_err()
            {
                let _ = Self::delete_slot(backend, scope, next_slot, index + 1);
                return Err(ChatGptOAuthError::KeyringUnavailable);
            }
        }

        let mut manifest = KeyringChunkManifest {
            slot: next_slot,
            chunks: chunks.len(),
            cleanup_pending: previous_manifest.is_some(),
        };
        if let Err(err) = Self::write_manifest(backend, scope, &manifest) {
            let _ = Self::delete_slot(backend, scope, next_slot, chunks.len());
            return Err(err);
        }
        Self::finish_pending_cleanup(backend, scope, &mut manifest)?;
        Ok(())
    }

    fn read_root(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
    ) -> Result<Option<String>, ChatGptOAuthError> {
        match backend.get_password(CHATGPT_OAUTH_KEYRING_SERVICE, &scope.root_key()) {
            Ok(raw) => Ok(Some(raw)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(ChatGptOAuthError::KeyringUnavailable),
        }
    }

    // Old builds could store a compact bundle in the root. Only a valid root
    // can be used to decide which slot it is safe to overwrite. A manifest with
    // `cleanup_pending == true` points at an already-committed active slot and
    // is recovered by finishing deletion of the stale opposite slot.
    fn parse_root(raw: &str) -> Result<Option<KeyringChunkManifest>, ChatGptOAuthError> {
        if let Ok(manifest) = serde_json::from_str::<KeyringChunkManifest>(raw) {
            if manifest.slot > 1 || !(1..=MAX_KEYRING_CHUNKS).contains(&manifest.chunks) {
                return Err(ChatGptOAuthError::KeyringUnavailable);
            }
            return Ok(Some(manifest));
        }
        serde_json::from_str::<StoredChatGptOAuthTokens>(raw)
            .map(|_| None)
            .map_err(|_| ChatGptOAuthError::KeyringUnavailable)
    }

    fn finish_pending_cleanup(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
        manifest: &mut KeyringChunkManifest,
    ) -> Result<(), ChatGptOAuthError> {
        if !manifest.cleanup_pending {
            return Ok(());
        }
        let stale_slot = 1 - manifest.slot;
        Self::delete_slot(backend, scope, stale_slot, MAX_KEYRING_CHUNKS)?;
        manifest.cleanup_pending = false;
        Self::write_manifest(backend, scope, manifest)
    }

    fn write_manifest(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
        manifest: &KeyringChunkManifest,
    ) -> Result<(), ChatGptOAuthError> {
        let raw =
            serde_json::to_string(manifest).map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
        backend
            .set_password(CHATGPT_OAUTH_KEYRING_SERVICE, &scope.root_key(), &raw)
            .map_err(|_| ChatGptOAuthError::KeyringUnavailable)
    }

    fn delete_entry(backend: &dyn CredentialBackend, key: &str) -> Result<(), ChatGptOAuthError> {
        match backend.delete_credential(CHATGPT_OAUTH_KEYRING_SERVICE, key) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(ChatGptOAuthError::KeyringUnavailable),
        }
    }

    fn delete_slot(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
        slot: u8,
        chunks: usize,
    ) -> Result<(), ChatGptOAuthError> {
        let mut result = Ok(());
        for index in 0..chunks {
            if let Err(err) = Self::delete_entry(backend, &scope.chunk_key(slot, index)) {
                result = Err(err);
            }
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn load_with(
        backend: &dyn CredentialBackend,
    ) -> Result<Option<StoredChatGptOAuthTokens>, ChatGptOAuthError> {
        Self::load_with_scope(backend, &KeyringScope::unscoped())
    }

    pub(crate) fn load_with_scope(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
    ) -> Result<Option<StoredChatGptOAuthTokens>, ChatGptOAuthError> {
        let Some(raw) = Self::read_root(backend, scope)? else {
            return Ok(None);
        };
        if let Some(mut manifest) = Self::parse_root(&raw)? {
            Self::finish_pending_cleanup(backend, scope, &mut manifest)?;
            let mut assembled = String::with_capacity(manifest.chunks * MAX_KEYRING_CHUNK_CHARS);
            for idx in 0..manifest.chunks {
                let chunk = backend
                    .get_password(
                        CHATGPT_OAUTH_KEYRING_SERVICE,
                        &scope.chunk_key(manifest.slot, idx),
                    )
                    .map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
                assembled.push_str(&chunk);
            }
            let parsed: StoredChatGptOAuthTokens = serde_json::from_str(&assembled)
                .map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
            return Ok(Some(parsed));
        }

        let parsed: StoredChatGptOAuthTokens =
            serde_json::from_str(&raw).map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
        Ok(Some(parsed))
    }

    /// Delete stored ChatGPT OAuth tokens from the OS keyring (idempotent).
    pub(crate) fn clear() -> Result<(), ChatGptOAuthError> {
        let scope = ensure_desktop_lease()?;
        let _guard = session_lock()?;
        DISCONNECT_REQUIRED.store(true, Ordering::SeqCst);
        invalidate_session()?;
        let tokens = Self::load_with_scope(&KeyringBackend, &scope);
        Self::mark_disconnect_pending(&KeyringBackend, &scope)?;
        let revocation = match tokens {
            Ok(Some(tokens)) => revoke_session(&ChatGptOAuthConfig::default(), &tokens),
            Ok(None) => Ok(()),
            Err(_) => Err(ChatGptOAuthError::RevocationUnconfirmed),
        };
        Self::clear_chunks(&KeyringBackend, &scope)?;
        DISCONNECT_REQUIRED.store(false, Ordering::SeqCst);
        revocation
    }

    #[cfg(test)]
    pub(crate) fn clear_with(backend: &dyn CredentialBackend) -> Result<(), ChatGptOAuthError> {
        Self::clear_with_scope(backend, &KeyringScope::unscoped())
    }

    #[cfg(test)]
    pub(crate) fn clear_with_scope(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
    ) -> Result<(), ChatGptOAuthError> {
        Self::mark_disconnect_pending(backend, scope)?;
        Self::clear_chunks(backend, scope)
    }

    fn mark_disconnect_pending(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
    ) -> Result<(), ChatGptOAuthError> {
        // Publish a durable deny marker before touching any chunks. A partial
        // delete stays recoverable after restart and cannot be used to infer.
        backend
            .set_password(
                CHATGPT_OAUTH_KEYRING_SERVICE,
                &scope.root_key(),
                DISCONNECT_PENDING,
            )
            .map_err(|_| ChatGptOAuthError::KeyringUnavailable)
    }

    fn clear_chunks(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
    ) -> Result<(), ChatGptOAuthError> {
        let first = Self::delete_slot(backend, scope, 0, MAX_KEYRING_CHUNKS);
        let second = Self::delete_slot(backend, scope, 1, MAX_KEYRING_CHUNKS);
        first.and(second)?;
        Self::delete_entry(backend, &scope.root_key())
    }

    /// Report secret-free connection status from the OS keyring.
    pub(crate) fn status() -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
        let scope = ensure_desktop_lease()?;
        Self::status_with_scope(&KeyringBackend, &scope)
    }

    #[cfg(test)]
    pub(crate) fn status_with(
        backend: &dyn CredentialBackend,
    ) -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
        Self::status_with_scope(backend, &KeyringScope::unscoped())
    }

    pub(crate) fn status_with_scope(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
    ) -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
        let _guard = session_lock()?;
        require_no_pending_disconnect()?;
        Ok(Self::load_with_scope(backend, scope)?
            .map_or_else(ChatGptOAuthStatus::disconnected, |t| t.status()))
    }

    pub(crate) fn is_connected_with(
        backend: &dyn CredentialBackend,
    ) -> Result<bool, ChatGptOAuthError> {
        Self::is_connected_with_scope(backend, &KeyringScope::unscoped())
    }

    pub(crate) fn is_connected_with_scope(
        backend: &dyn CredentialBackend,
        scope: &KeyringScope,
    ) -> Result<bool, ChatGptOAuthError> {
        Ok(Self::status_with_scope(backend, scope)?.connected)
    }
}

/// PKCE `S256` verifier + challenge pair (RFC 7636).
#[derive(Clone)]
pub(crate) struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for PkcePair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkcePair")
            .field("verifier", &"[REDACTED]")
            .field("challenge", &self.challenge)
            .finish()
    }
}

/// Generate a 256-bit cryptographically random URL-safe token (`Uuid::new_v4` x 2
/// = 32 random bytes -> 43 base64url no-pad chars).
#[must_use]
fn random_token_256bit() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute RFC 7636 `S256` PKCE challenge: `BASE64URL-ENCODE(SHA256(ASCII(verifier)))`.
#[must_use]
pub(crate) fn compute_s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Generate a fresh PKCE `S256` verifier and challenge pair.
#[must_use]
pub(crate) fn generate_pkce() -> PkcePair {
    let verifier = random_token_256bit();
    let challenge = compute_s256_challenge(&verifier);
    PkcePair {
        verifier,
        challenge,
    }
}

/// Generate a fresh 256-bit CSRF `state` token.
#[must_use]
pub(crate) fn generate_csrf_state() -> String {
    random_token_256bit()
}

/// Constant-time byte slice equality for CSRF `state` verification.
#[must_use]
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Verify that the OAuth callback's `state` query parameter matches
/// `expected_state` in constant time.
///
/// # Errors
/// Returns [`ChatGptOAuthError::StateMismatch`] when `received_state` is
/// missing or does not match `expected_state`.
pub(crate) fn verify_csrf_state(
    received_state: Option<&str>,
    expected_state: &str,
) -> Result<(), ChatGptOAuthError> {
    match received_state {
        Some(received) if constant_time_eq(received.as_bytes(), expected_state.as_bytes()) => {
            Ok(())
        }
        _ => Err(ChatGptOAuthError::StateMismatch),
    }
}

/// Build the OAuth 2.0 + PKCE `S256` authorization URL with CSRF `state`.
///
/// # Errors
/// Returns [`ChatGptOAuthError::InvalidEndpoint`] if the configured issuer URL
/// is invalid or insecure.
pub(crate) fn build_authorization_url(
    config: &ChatGptOAuthConfig,
    redirect_uri: &str,
    pkce: &PkcePair,
    state: &str,
    nonce: &str,
    host_id: &str,
    identity: Option<&PlanIdentity>,
) -> Result<String, ChatGptOAuthError> {
    let mut url = config.authorize_endpoint()?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("response_type", "code");
        if let Some(identity) = identity {
            pairs.append_pair("client_id", &identity.client_id);
            pairs.append_pair("id_token_hint", &identity.id_token);
        } else {
            pairs.append_pair("client_id", config.client_id.trim());
            pairs.append_pair("agent_name_hint", "Nexora");
        }
        pairs.append_pair("ext_agent_host_id", host_id);
        pairs.append_pair("nonce", nonce);
        pairs.append_pair("resource", PLAN_RESOURCE);
        pairs.append_pair("redirect_uri", redirect_uri);
        pairs.append_pair("code_challenge", &pkce.challenge);
        pairs.append_pair("code_challenge_method", "S256");
        pairs.append_pair("state", state);
        if !config.scopes.trim().is_empty() {
            pairs.append_pair("scope", config.scopes.trim());
        }
    }
    Ok(url.into())
}

pub(crate) struct AuthorizationCallback {
    code: String,
    client_id: Option<String>,
}

impl std::fmt::Debug for AuthorizationCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationCallback")
            .finish_non_exhaustive()
    }
}

/// Loopback OAuth callback listener bound strictly to `127.0.0.1`.
pub(crate) struct LoopbackCallbackServer {
    listener: TcpListener,
    addr: SocketAddr,
}

impl LoopbackCallbackServer {
    /// Bind the first available port in `preferred_ports` on `127.0.0.1`,
    /// falling back to an ephemeral port (`127.0.0.1:0`).
    ///
    /// # Errors
    /// Returns [`ChatGptOAuthError::LoopbackBindFailed`] if no loopback socket
    /// can be bound.
    pub(crate) fn bind(preferred_ports: &[u16]) -> Result<Self, ChatGptOAuthError> {
        let mut bound = None;
        for &port in preferred_ports {
            if port == 0 {
                continue;
            }
            if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
                bound = Some(listener);
                break;
            }
        }
        let listener = match bound {
            Some(l) => l,
            None => TcpListener::bind("127.0.0.1:0")
                .map_err(|_| ChatGptOAuthError::LoopbackBindFailed)?,
        };
        listener
            .set_nonblocking(true)
            .map_err(|_| ChatGptOAuthError::LoopbackBindFailed)?;
        let addr = listener
            .local_addr()
            .map_err(|_| ChatGptOAuthError::LoopbackBindFailed)?;
        Ok(Self { listener, addr })
    }

    #[must_use]
    pub(crate) fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/auth/callback", self.addr.port())
    }

    /// Wait for the OAuth callback on `127.0.0.1`, verifying `state` in
    /// constant time and returning the single-use authorization `code`.
    ///
    /// Unauthenticated requests with missing or mismatched CSRF `state`
    /// receive HTTP `400 Bad Request` without aborting the listener.
    ///
    /// # Errors
    /// Returns [`ChatGptOAuthError::CallbackTimeout`],
    /// [`ChatGptOAuthError::AuthorizationDenied`],
    /// [`ChatGptOAuthError::MissingCode`], or [`ChatGptOAuthError::Cancelled`].
    #[cfg(test)]
    pub(crate) fn wait_for_code(
        &self,
        expected_state: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<AuthorizationCallback, ChatGptOAuthError> {
        self.wait_for_code_with_signals(expected_state, timeout, cancel, None, None)
    }

    fn wait_for_code_with_signals(
        &self,
        expected_state: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
        session_cancel: Option<&AtomicBool>,
        expected_epoch: Option<u64>,
    ) -> Result<AuthorizationCallback, ChatGptOAuthError> {
        let is_cancelled = || {
            cancel.is_some_and(|flag| flag.load(Ordering::SeqCst))
                || session_cancel.is_some_and(|flag| flag.load(Ordering::SeqCst))
                || expected_epoch.is_some_and(|epoch| SESSION_EPOCH.load(Ordering::SeqCst) != epoch)
        };
        let deadline = Instant::now() + timeout;
        loop {
            if is_cancelled() {
                return Err(ChatGptOAuthError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(ChatGptOAuthError::CallbackTimeout);
            }
            match self.listener.accept() {
                Ok((mut stream, _peer)) => {
                    if let Some(outcome) = handle_callback_connection(
                        &mut stream,
                        expected_state,
                        deadline,
                        &is_cancelled,
                    ) {
                        return outcome;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => return Err(ChatGptOAuthError::LoopbackBindFailed),
            }
        }
    }
}

fn handle_callback_connection(
    stream: &mut TcpStream,
    expected_state: &str,
    deadline: Instant,
    is_cancelled: &dyn Fn() -> bool,
) -> Option<Result<AuthorizationCallback, ChatGptOAuthError>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));

    let mut buf = [0u8; MAX_CALLBACK_REQUEST_BYTES];
    let mut filled = 0usize;
    while filled < MAX_CALLBACK_REQUEST_BYTES {
        if is_cancelled() {
            return Some(Err(ChatGptOAuthError::Cancelled));
        }
        let now = Instant::now();
        if now >= deadline {
            return Some(Err(ChatGptOAuthError::CallbackTimeout));
        }
        let read_timeout = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(50))
            .max(Duration::from_millis(1));
        let _ = stream.set_read_timeout(Some(read_timeout));
        match stream.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n")
                    || buf[..filled].windows(2).any(|w| w == b"\n\n")
                {
                    break;
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    let request = String::from_utf8_lossy(&buf[..filled]);
    let Some(target) = parse_http_get_target(&request) else {
        write_static_http_response(stream, "400 Bad Request", "Invalid OAuth callback request.");
        return None;
    };

    let Ok(parsed) = Url::parse(&format!("http://127.0.0.1{target}")) else {
        write_static_http_response(stream, "400 Bad Request", "Malformed OAuth callback URL.");
        return None;
    };

    if parsed.path() != "/auth/callback" {
        write_static_http_response(stream, "404 Not Found", "Not found.");
        return None;
    }

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut error: Option<String> = None;
    let mut client_id = None;
    for (k, v) in parsed.query_pairs() {
        let target = match k.as_ref() {
            "code" => &mut code,
            "state" => &mut state,
            "error" => &mut error,
            "client_id" => &mut client_id,
            _ => continue,
        };
        if target.replace(v.into_owned()).is_some() {
            write_static_http_response(stream, "400 Bad Request", "Invalid OAuth callback.");
            return None;
        }
    }

    if verify_csrf_state(state.as_deref(), expected_state).is_err() {
        write_static_http_response(
            stream,
            "400 Bad Request",
            "OAuth state verification failed. You may close this tab.",
        );
        return None;
    }

    if error.is_some() {
        write_static_http_response(
            stream,
            "400 Bad Request",
            "Sign-in was denied or cancelled. You may close this tab.",
        );
        return Some(Err(ChatGptOAuthError::AuthorizationDenied));
    }

    let Some(auth_code) = code.filter(|c| !c.trim().is_empty()) else {
        write_static_http_response(
            stream,
            "400 Bad Request",
            "Missing authorization code. You may close this tab.",
        );
        return Some(Err(ChatGptOAuthError::MissingCode));
    };

    write_static_http_response(
        stream,
        "200 OK",
        "Return to Nexora to check the result of sign-in.",
    );
    Some(Ok(AuthorizationCallback {
        code: auth_code,
        client_id,
    }))
}

fn parse_http_get_target(request: &str) -> Option<&str> {
    let first_line = request.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if method != "GET" {
        return None;
    }
    Some(target)
}

fn write_static_http_response(stream: &mut TcpStream, status: &str, body_text: &str) {
    let html = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Nexora Sign-In</title></head>\
         <body><p>{body_text}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n\
         {html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

#[derive(Deserialize)]
struct RawTokenResponse {
    access_token: String,
    id_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Machine-readable OAuth 2.0 token endpoint error codes (RFC 6749 §5.2).
///
/// Only the standard machine-readable `error` code is classified; raw
/// `error_description` strings and response bodies are never retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OAuthTokenErrorCode {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnauthorizedClient,
    UnsupportedGrantType,
    InvalidScope,
    AccessDenied,
    TemporarilyUnavailable,
    ServerError,
    Unknown,
}

impl OAuthTokenErrorCode {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "invalid_request" => Self::InvalidRequest,
            "invalid_client" => Self::InvalidClient,
            "invalid_grant" => Self::InvalidGrant,
            "unauthorized_client" => Self::UnauthorizedClient,
            "unsupported_grant_type" => Self::UnsupportedGrantType,
            "invalid_scope" => Self::InvalidScope,
            "access_denied" => Self::AccessDenied,
            "temporarily_unavailable" => Self::TemporarilyUnavailable,
            "server_error" => Self::ServerError,
            _ => Self::Unknown,
        }
    }

    const fn is_terminal_grant_failure(self) -> bool {
        matches!(
            self,
            Self::InvalidGrant | Self::UnauthorizedClient | Self::InvalidScope | Self::AccessDenied
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenEndpointError {
    Transport,
    ResponseTooLarge,
    InvalidPayload,
    Http {
        status: u16,
        code: OAuthTokenErrorCode,
    },
}

impl TokenEndpointError {
    const fn is_transient(self) -> bool {
        match self {
            Self::Transport => true,
            Self::Http { status, code } => {
                status == 429
                    || status >= 500
                    || matches!(
                        code,
                        OAuthTokenErrorCode::TemporarilyUnavailable
                            | OAuthTokenErrorCode::ServerError
                    )
            }
            Self::ResponseTooLarge | Self::InvalidPayload => false,
        }
    }
}

#[derive(Deserialize)]
struct OAuthErrorBody {
    #[serde(default)]
    error: Option<String>,
}

fn classify_token_http_error(status: reqwest::StatusCode, body: &[u8]) -> TokenEndpointError {
    let parsed_code = serde_json::from_slice::<OAuthErrorBody>(body)
        .ok()
        .and_then(|payload| payload.error)
        .map(|code| OAuthTokenErrorCode::parse(&code));
    let code = parsed_code.unwrap_or_else(|| {
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            OAuthTokenErrorCode::TemporarilyUnavailable
        } else if status.is_server_error() {
            OAuthTokenErrorCode::ServerError
        } else {
            OAuthTokenErrorCode::Unknown
        }
    });
    TokenEndpointError::Http {
        status: status.as_u16(),
        code,
    }
}

fn post_token_form(
    token_url: Url,
    form: &[(&str, &str)],
    timeout: Duration,
) -> Result<RawTokenResponse, TokenEndpointError> {
    let client = oauth_client(timeout, ChatGptOAuthError::TokenExchangeFailed)
        .map_err(|_| TokenEndpointError::Transport)?;
    let encoded_form: String = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form.iter().copied())
        .finish();
    let response = client
        .post(token_url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encoded_form)
        .send()
        .map_err(|_| TokenEndpointError::Transport)?;

    let status = response.status();
    let mut body = Vec::new();
    response
        .take(MAX_TOKEN_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| TokenEndpointError::Transport)?;
    if body.len() > MAX_TOKEN_RESPONSE_BYTES {
        return Err(TokenEndpointError::ResponseTooLarge);
    }
    if !status.is_success() {
        return Err(classify_token_http_error(status, &body));
    }
    serde_json::from_slice(&body).map_err(|_| TokenEndpointError::InvalidPayload)
}

fn oauth_client(
    timeout: Duration,
    failure_kind: ChatGptOAuthError,
) -> Result<reqwest::blocking::Client, ChatGptOAuthError> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| failure_kind)
}

fn read_oauth_bytes(
    response: reqwest::blocking::Response,
    failure_kind: ChatGptOAuthError,
) -> Result<Vec<u8>, ChatGptOAuthError> {
    if !response.status().is_success() {
        return Err(failure_kind);
    }

    let mut body = Vec::new();
    response
        .take(MAX_TOKEN_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| failure_kind)?;
    if body.len() > MAX_TOKEN_RESPONSE_BYTES {
        return Err(ChatGptOAuthError::ResponseTooLarge);
    }

    Ok(body)
}

fn read_oauth_json<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
    failure_kind: ChatGptOAuthError,
) -> Result<T, ChatGptOAuthError> {
    let body = read_oauth_bytes(response, failure_kind)?;
    serde_json::from_slice(&body).map_err(|_| failure_kind)
}

#[derive(Deserialize)]
struct OpenIdDiscovery {
    revocation_endpoint: String,
}

fn revoke_session(
    config: &ChatGptOAuthConfig,
    tokens: &StoredChatGptOAuthTokens,
) -> Result<(), ChatGptOAuthError> {
    let (Some(identity), Some(refresh)) = (&tokens.identity, &tokens.refresh_token) else {
        return Ok(());
    };
    let failure = ChatGptOAuthError::RevocationUnconfirmed;
    let issuer = config.issuer.trim_end_matches('/');
    let client = oauth_client(DEFAULT_OAUTH_HTTP_TIMEOUT, failure)?;
    let discovery_url = format!("{issuer}/.well-known/openid-configuration");
    let discovery: OpenIdDiscovery = read_oauth_json(
        client.get(&discovery_url).send().map_err(|_| failure)?,
        failure,
    )?;
    let endpoint =
        validate_oauth_endpoint_url(&discovery.revocation_endpoint).map_err(|_| failure)?;
    let issuer = validate_oauth_endpoint_url(issuer).map_err(|_| failure)?;
    if endpoint.origin() != issuer.origin() || !is_issued_client(&identity.client_id) {
        return Err(failure);
    }
    let form = [
        ("token", refresh.as_str()),
        ("token_type_hint", "refresh_token"),
        ("client_id", identity.client_id.as_str()),
    ];
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    for delay in [Duration::ZERO, Duration::from_millis(200)] {
        std::thread::sleep(delay);
        match client
            .post(endpoint.clone())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.clone())
            .send()
        {
            Ok(response) if response.status() == reqwest::StatusCode::OK => return Ok(()),
            Ok(response) if !response.status().is_server_error() => break,
            _ => {}
        }
    }
    Err(failure)
}

fn make_record(
    raw: RawTokenResponse,
    identity: PlanIdentity,
    failure_kind: ChatGptOAuthError,
) -> Result<StoredChatGptOAuthTokens, ChatGptOAuthError> {
    if raw.access_token.trim().is_empty()
        || raw.access_token.chars().any(char::is_control)
        || raw
            .token_type
            .as_deref()
            .is_none_or(|kind| !kind.eq_ignore_ascii_case("bearer"))
        || raw.expires_in.is_none_or(|secs| secs <= 0)
    {
        return Err(failure_kind);
    }
    let now = now_unix_secs();
    let expires_at = raw
        .expires_in
        .filter(|&secs| secs > 0)
        .map(|secs| now.saturating_add(secs));

    Ok(StoredChatGptOAuthTokens {
        access_token: raw.access_token,
        refresh_token: raw.refresh_token.filter(|s| !s.trim().is_empty()),
        expires_at,
        token_type: raw.token_type,
        scope: raw.scope,
        connected_at: now,
        identity: Some(identity),
        reauthorization_required: false,
    })
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<RsaJwk>,
}

#[derive(Deserialize)]
struct RsaJwk {
    kid: String,
    kty: String,
    n: Option<String>,
    e: Option<String>,
    alg: Option<String>,
}

#[derive(Deserialize)]
struct IdHeader {
    alg: String,
    kid: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Deserialize)]
struct IdClaims {
    iss: String,
    aud: Audience,
    azp: Option<String>,
    sub: String,
    nonce: String,
    exp: i64,
    iat: i64,
}

fn verify_id_token(
    config: &ChatGptOAuthConfig,
    token: &str,
    client_id: &str,
    nonce: &str,
    timeout: Duration,
) -> Result<String, ChatGptOAuthError> {
    let failure = ChatGptOAuthError::IdTokenVerificationFailed;
    let jwks_failure = ChatGptOAuthError::JwksFetchFailed;
    let (signed, signature) = token.rsplit_once('.').ok_or(failure)?;
    let (header, claims) = signed.split_once('.').ok_or(failure)?;
    let decode = |value| URL_SAFE_NO_PAD.decode(value).map_err(|_| failure);
    let header: IdHeader = serde_json::from_slice(&decode(header)?).map_err(|_| failure)?;
    if header.alg != "RS256" || header.kid.is_empty() {
        return Err(failure);
    }
    let endpoint = validate_oauth_endpoint_url(&format!(
        "{}/.well-known/jwks.json",
        config.issuer.trim_end_matches('/')
    ))?;
    let response = oauth_client(timeout, jwks_failure)?
        .get(endpoint)
        .send()
        .map_err(|_| jwks_failure)?;
    let keys: JwkSet = read_oauth_json(response, jwks_failure)?;
    let key = keys
        .keys
        .iter()
        .find(|key| {
            key.kid == header.kid
                && key.kty == "RSA"
                && key.alg.as_deref().is_none_or(|alg| alg == "RS256")
        })
        .ok_or(failure)?;
    let public = ring::signature::RsaPublicKeyComponents {
        n: decode(key.n.as_deref().ok_or(failure)?)?,
        e: decode(key.e.as_deref().ok_or(failure)?)?,
    };
    public
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            signed.as_bytes(),
            &decode(signature)?,
        )
        .map_err(|_| failure)?;

    let claims: IdClaims = serde_json::from_slice(&decode(claims)?).map_err(|_| failure)?;
    let audience_matches = match &claims.aud {
        Audience::Single(aud) => aud == client_id,
        Audience::Multiple(aud) => {
            aud.iter().any(|aud| aud == client_id)
                && (aud.len() == 1 || claims.azp.as_deref() == Some(client_id))
        }
    };
    let now = now_unix_secs();
    if !audience_matches
        || claims.azp.as_deref().is_some_and(|azp| azp != client_id)
        || claims.iss != config.issuer
        || claims.sub.trim().is_empty()
        || claims.exp <= now
        || claims.iat > now.saturating_add(ID_TOKEN_CLOCK_SKEW_SECS)
        || !constant_time_eq(claims.nonce.as_bytes(), nonce.as_bytes())
    {
        return Err(failure);
    }
    Ok(claims.sub)
}

#[cfg(test)]
fn host_id(backend: &dyn CredentialBackend) -> Result<String, ChatGptOAuthError> {
    host_id_with_scope(backend, &KeyringScope::unscoped())
}

fn host_id_with_scope(
    backend: &dyn CredentialBackend,
    scope: &KeyringScope,
) -> Result<String, ChatGptOAuthError> {
    let key = scope.host_id_key();
    match backend.get_password(CHATGPT_OAUTH_KEYRING_SERVICE, &key) {
        Ok(host) => {
            host.strip_prefix("urn:uuid:")
                .and_then(|uuid| Uuid::parse_str(uuid).ok())
                .ok_or(ChatGptOAuthError::KeyringUnavailable)?;
            Ok(host)
        }
        Err(keyring::Error::NoEntry) => {
            let host = format!("urn:uuid:{}", Uuid::new_v4());
            backend
                .set_password(CHATGPT_OAUTH_KEYRING_SERVICE, &key, &host)
                .map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
            Ok(host)
        }
        Err(_) => Err(ChatGptOAuthError::KeyringUnavailable),
    }
}

#[derive(Deserialize)]
struct AccountModels {
    models: Vec<AccountModelEntry>,
}

#[derive(Deserialize)]
struct AccountModelEntry {
    #[serde(flatten)]
    model: PlanModel,
    visibility: String,
}

#[derive(Deserialize)]
struct OpenAiModels {
    data: Vec<OpenAiModelEntry>,
}

#[derive(Deserialize)]
struct OpenAiModelEntry {
    id: String,
}

fn baseline_plan_models() -> Vec<PlanModel> {
    vec![
        PlanModel {
            slug: "gpt-5".to_owned(),
            display_name: "GPT-5".to_owned(),
        },
        PlanModel {
            slug: "gpt-4o".to_owned(),
            display_name: "GPT-4o".to_owned(),
        },
        PlanModel {
            slug: "o3".to_owned(),
            display_name: "o3".to_owned(),
        },
        PlanModel {
            slug: "o4-mini".to_owned(),
            display_name: "o4-mini".to_owned(),
        },
    ]
}

fn is_valid_plan_model(model: &PlanModel) -> bool {
    !model.slug.is_empty()
        && model.slug.len() <= 128
        && model
            .slug
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.' | b':'))
        && !model.display_name.trim().is_empty()
        && model.display_name.len() <= 128
        && !model.display_name.chars().any(char::is_control)
}

fn parse_plan_models(body: &[u8]) -> Result<Vec<PlanModel>, ChatGptOAuthError> {
    let mut models = Vec::new();
    if let Ok(catalog) = serde_json::from_slice::<AccountModels>(body) {
        for entry in catalog.models {
            let model = entry.model;
            if entry.visibility == "list"
                && is_valid_plan_model(&model)
                && !models
                    .iter()
                    .any(|existing: &PlanModel| existing.slug == model.slug)
            {
                models.push(model);
            }
        }
        return Ok(models);
    }
    if let Ok(catalog) = serde_json::from_slice::<OpenAiModels>(body) {
        for entry in catalog.data {
            let model = PlanModel {
                slug: entry.id.clone(),
                display_name: entry.id,
            };
            if is_valid_plan_model(&model)
                && !models
                    .iter()
                    .any(|existing: &PlanModel| existing.slug == model.slug)
            {
                models.push(model);
            }
        }
        return Ok(models);
    }
    Err(ChatGptOAuthError::TokenExchangeFailed)
}

fn list_plan_models(
    config: &ChatGptOAuthConfig,
    access_token: &str,
    timeout: Duration,
) -> Result<Vec<PlanModel>, ChatGptOAuthError> {
    let failure = ChatGptOAuthError::TokenExchangeFailed;
    let _ = config;
    #[cfg(not(test))]
    let url = format!("{PLAN_RESOURCE}/models");
    #[cfg(test)]
    let url = format!("{}/v1/models", config.issuer);
    let response = oauth_client(timeout, failure)?
        .get(url)
        .bearer_auth(access_token)
        .send()
        .map_err(|_| failure)?;
    let body = read_oauth_bytes(response, failure)?;
    parse_plan_models(&body)
}

fn resolve_plan_models(
    config: &ChatGptOAuthConfig,
    access_token: &str,
    timeout: Duration,
) -> Vec<PlanModel> {
    match list_plan_models(config, access_token, timeout) {
        Ok(models) if !models.is_empty() => models,
        _ => baseline_plan_models(),
    }
}

struct ExchangeError {
    error: ChatGptOAuthError,
    verified_identity: Option<PlanIdentity>,
}

impl From<ChatGptOAuthError> for ExchangeError {
    fn from(error: ChatGptOAuthError) -> Self {
        Self {
            error,
            verified_identity: None,
        }
    }
}

fn exchange_code(
    config: &ChatGptOAuthConfig,
    callback: AuthorizationCallback,
    redirect_uri: &str,
    code_verifier: &str,
    nonce: &str,
    previous: Option<&PlanIdentity>,
    timeout: Duration,
) -> Result<StoredChatGptOAuthTokens, ExchangeError> {
    let failure = ChatGptOAuthError::TokenExchangeFailed;
    let client_id = match previous {
        Some(identity) => {
            if callback
                .client_id
                .as_ref()
                .is_some_and(|id| id != &identity.client_id)
            {
                return Err(ChatGptOAuthError::CallbackClientIdInvalid.into());
            }
            identity.client_id.clone()
        }
        None => callback
            .client_id
            .or_else(|| {
                let configured = config.client_id.trim();
                (configured != DYNAMIC_CLIENT_ID && !configured.is_empty())
                    .then(|| configured.to_owned())
            })
            .filter(|id| is_issued_client(id))
            .ok_or(ChatGptOAuthError::CallbackClientIdInvalid)?,
    };
    let form = [
        ("grant_type", "authorization_code"),
        ("code", callback.code.as_str()),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id.as_str()),
        ("code_verifier", code_verifier),
        ("resource", PLAN_RESOURCE),
    ];
    let mut raw = post_token_form(config.token_endpoint()?, &form, timeout).map_err(|err| {
        let mapped = match err {
            TokenEndpointError::ResponseTooLarge => ChatGptOAuthError::ResponseTooLarge,
            TokenEndpointError::Http {
                code: OAuthTokenErrorCode::AccessDenied | OAuthTokenErrorCode::InvalidScope,
                ..
            } => ChatGptOAuthError::AuthorizationDenied,
            TokenEndpointError::Http { status, .. } => {
                ChatGptOAuthError::TokenEndpointRejected { status }
            }
            TokenEndpointError::Transport | TokenEndpointError::InvalidPayload => failure,
        };
        ExchangeError::from(mapped)
    })?;
    let id_token = raw
        .id_token
        .take()
        .ok_or(ChatGptOAuthError::IdTokenVerificationFailed)?;
    let subject = verify_id_token(config, &id_token, &client_id, nonce, timeout)?;
    if previous.is_some_and(|previous| previous.subject != subject) {
        return Err(ChatGptOAuthError::IdTokenVerificationFailed.into());
    }
    let verified_identity = PlanIdentity {
        client_id,
        subject,
        id_token,
        models: Vec::new(),
    };
    if !has_plan_scope(raw.scope.as_deref()) {
        return Err(ExchangeError {
            error: ChatGptOAuthError::AuthorizationDenied,
            verified_identity: Some(verified_identity),
        });
    }
    let mut record = match make_record(raw, verified_identity.clone(), failure) {
        Ok(record) => record,
        Err(error) => {
            return Err(ExchangeError {
                error,
                verified_identity: Some(verified_identity),
            });
        }
    };
    let models = resolve_plan_models(config, &record.access_token, timeout);
    if let Some(identity) = record.identity.as_mut() {
        identity.models = models;
    }
    Ok(record)
}

fn mark_reauthorization_required_locked(
    backend: &dyn CredentialBackend,
    scope: &KeyringScope,
    identity: PlanIdentity,
) -> Result<(), ChatGptOAuthError> {
    let unusable = StoredChatGptOAuthTokens::reauthorization_required(identity);
    ChatGptOAuthStore::save_with_scope(backend, scope, &unusable)?;
    invalidate_session()
}

fn refresh_tokens_locked(
    backend: &dyn CredentialBackend,
    scope: &KeyringScope,
    config: &ChatGptOAuthConfig,
    timeout: Duration,
) -> Result<StoredChatGptOAuthTokens, ChatGptOAuthError> {
    let existing = ChatGptOAuthStore::load_with_scope(backend, scope)?
        .ok_or(ChatGptOAuthError::NotConnected)?;
    if existing.reauthorization_required {
        return Err(ChatGptOAuthError::ReauthorizationRequired);
    }
    let identity = existing
        .identity
        .clone()
        .filter(|identity| is_issued_client(&identity.client_id))
        .ok_or(ChatGptOAuthError::NotConnected)?;
    let Some(refresh) = existing
        .refresh_token
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    else {
        mark_reauthorization_required_locked(backend, scope, identity)?;
        return Err(ChatGptOAuthError::TokenRefreshFailed);
    };

    let failure = ChatGptOAuthError::TokenRefreshFailed;
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", identity.client_id.as_str()),
        ("resource", PLAN_RESOURCE),
    ];
    let mut raw = match post_token_form(config.token_endpoint()?, &form, timeout) {
        Ok(raw) => raw,
        Err(TokenEndpointError::ResponseTooLarge) => {
            return Err(ChatGptOAuthError::ResponseTooLarge);
        }
        Err(TokenEndpointError::Http { code, .. }) if code.is_terminal_grant_failure() => {
            mark_reauthorization_required_locked(backend, scope, identity)?;
            return Err(ChatGptOAuthError::ReauthorizationRequired);
        }
        Err(TokenEndpointError::Http {
            code: OAuthTokenErrorCode::InvalidClient,
            ..
        }) => {
            ChatGptOAuthStore::mark_disconnect_pending(backend, scope)?;
            ChatGptOAuthStore::clear_chunks(backend, scope)?;
            invalidate_session()?;
            return Err(ChatGptOAuthError::TokenRefreshFailed);
        }
        Err(err) if err.is_transient() => {
            return Err(ChatGptOAuthError::TokenRefreshTransient);
        }
        Err(_) => return Err(failure),
    };
    if raw.scope.is_none() {
        raw.scope.clone_from(&existing.scope);
    }
    if !has_plan_scope(raw.scope.as_deref()) {
        mark_reauthorization_required_locked(backend, scope, identity)?;
        return Err(ChatGptOAuthError::AuthorizationDenied);
    }
    let mut updated = make_record(raw, identity, failure)?;
    // Preserve previous refresh token and initial connected_at timestamp if not rotated.
    if updated.refresh_token.is_none() {
        updated.refresh_token = existing.refresh_token;
    }
    if existing.connected_at > 0 {
        updated.connected_at = existing.connected_at;
    }
    if let Some(identity) = updated.identity.as_mut() {
        if identity.models.is_empty() {
            identity.models = baseline_plan_models();
        }
    }
    ChatGptOAuthStore::save_with_scope(backend, scope, &updated)?;
    Ok(updated)
}

/// Refresh stored ChatGPT OAuth tokens using the OS keyring `refresh_token` and
/// persist the rotated tokens exclusively in the OS keyring.
///
/// Guarded by a single-flight lock that re-reads the keyring inside the lock
/// before refreshing so concurrent callers never race on a rotating
/// `refresh_token`.
///
/// # Errors
/// Returns a secret-free [`ChatGptOAuthError`].
#[cfg(test)]
pub(crate) fn refresh_tokens_with(
    backend: &dyn CredentialBackend,
    config: &ChatGptOAuthConfig,
    timeout: Duration,
) -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
    refresh_tokens_with_scope(backend, &KeyringScope::unscoped(), config, timeout)
}

pub(crate) fn refresh_tokens_with_scope(
    backend: &dyn CredentialBackend,
    scope: &KeyringScope,
    config: &ChatGptOAuthConfig,
    timeout: Duration,
) -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
    let _guard = session_lock()?;
    require_no_pending_disconnect()?;
    let updated = refresh_tokens_locked(backend, scope, config, timeout)?;
    if updated.status().connected {
        Ok(updated.status())
    } else {
        Err(ChatGptOAuthError::AuthorizationDenied)
    }
}

/// Refresh stored ChatGPT OAuth tokens in production using the default OS
/// keyring and issuer config.
///
/// # Errors
/// Returns a secret-free [`ChatGptOAuthError`].
pub(crate) fn refresh_tokens() -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
    let scope = ensure_desktop_lease()?;
    refresh_tokens_with_scope(
        &KeyringBackend,
        &scope,
        &ChatGptOAuthConfig::default(),
        DEFAULT_OAUTH_HTTP_TIMEOUT,
    )
}

/// Give the request dispatcher a process-local reference to the signed-in
/// session. Only the Responses executor resolves the bearer, at each send.
pub(crate) fn session_reference_with(
    backend: &dyn CredentialBackend,
) -> Result<Option<String>, ChatGptOAuthError> {
    session_reference_with_scope(backend, &KeyringScope::unscoped())
}

pub(crate) fn session_reference_with_scope(
    backend: &dyn CredentialBackend,
    scope: &KeyringScope,
) -> Result<Option<String>, ChatGptOAuthError> {
    let _guard = session_lock()?;
    require_no_pending_disconnect()?;
    Ok(ChatGptOAuthStore::load_with_scope(backend, scope)?
        .filter(|record| record.status().connected)
        .map(|_| SESSION_EPOCH.load(Ordering::SeqCst).to_string()))
}

pub(crate) fn authorize_call(
    session: &str,
    model: &str,
    token: &CancellationToken,
) -> Result<ActivePlanCall, ChatGptOAuthError> {
    let scope = ensure_desktop_lease()?;
    let _guard = session_lock()?;
    require_no_pending_disconnect()?;
    if session.parse::<u64>().ok() != Some(SESSION_EPOCH.load(Ordering::SeqCst)) {
        return Err(ChatGptOAuthError::NotConnected);
    }
    let config = ChatGptOAuthConfig::default();
    let record =
        resolve_record_locked(&KeyringBackend, &scope, &config, DEFAULT_OAUTH_HTTP_TIMEOUT)?
            .ok_or(ChatGptOAuthError::NotConnected)?;
    if !record
        .identity
        .as_ref()
        .is_some_and(|identity| identity.models.iter().any(|entry| entry.slug == model))
    {
        return Err(ChatGptOAuthError::AuthorizationDenied);
    }
    let access_token = record.access_token;
    let mut active = ACTIVE_CALLS
        .lock()
        .map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
    let id = Uuid::new_v4();
    active.push((id, token.clone()));
    Ok(ActivePlanCall { id, access_token })
}

#[cfg(test)]
fn resolve_access_token_with(
    backend: &dyn CredentialBackend,
    config: &ChatGptOAuthConfig,
    timeout: Duration,
) -> Result<Option<String>, ChatGptOAuthError> {
    let _guard = session_lock()?;
    Ok(
        resolve_record_locked(backend, &KeyringScope::unscoped(), config, timeout)?
            .map(|record| record.access_token),
    )
}

fn resolve_record_locked(
    backend: &dyn CredentialBackend,
    scope: &KeyringScope,
    config: &ChatGptOAuthConfig,
    timeout: Duration,
) -> Result<Option<StoredChatGptOAuthTokens>, ChatGptOAuthError> {
    let Some(current) = ChatGptOAuthStore::load_with_scope(backend, scope)? else {
        return Ok(None);
    };
    if current.reauthorization_required {
        return Err(ChatGptOAuthError::ReauthorizationRequired);
    }
    let status = current.status();
    if status.problem == Some(ChatGptOAuthProblem::ReauthorizationRequired) {
        return refresh_tokens_locked(backend, scope, config, timeout).map(Some);
    }
    if !status.connected {
        return Ok(None);
    }
    if !current.needs_refresh(now_unix_secs()) {
        return Ok(Some(current));
    }

    let refreshed = refresh_tokens_locked(backend, scope, config, timeout)?;
    if refreshed.status().connected {
        Ok(Some(refreshed))
    } else {
        Err(ChatGptOAuthError::AuthorizationDenied)
    }
}

/// Run the full interactive opt-in ChatGPT OAuth 2.0 + PKCE loopback login flow
/// against `backend` and `config`.
///
/// # Errors
/// Returns a secret-free [`ChatGptOAuthError`].
#[cfg(test)]
pub(crate) fn run_oauth_login_flow_with<F>(
    backend: &dyn CredentialBackend,
    config: &ChatGptOAuthConfig,
    open_browser: F,
    callback_timeout: Duration,
    http_timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<ChatGptOAuthStatus, ChatGptOAuthError>
where
    F: FnOnce(&str) -> Result<(), ChatGptOAuthError>,
{
    run_oauth_login_flow_with_scope(
        backend,
        &KeyringScope::unscoped(),
        config,
        open_browser,
        callback_timeout,
        http_timeout,
        cancel,
    )
}

pub(crate) fn run_oauth_login_flow_with_scope<F>(
    backend: &dyn CredentialBackend,
    scope: &KeyringScope,
    config: &ChatGptOAuthConfig,
    open_browser: F,
    callback_timeout: Duration,
    http_timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<ChatGptOAuthStatus, ChatGptOAuthError>
where
    F: FnOnce(&str) -> Result<(), ChatGptOAuthError>,
{
    let _sign_in = BROWSER_SIGN_IN
        .try_lock()
        .map_err(|_| ChatGptOAuthError::Cancelled)?;
    let sign_in_cancel = SignInCancelGuard::register()?;
    // Validate endpoints up front before binding sockets or opening the browser.
    let _ = config.authorize_endpoint()?;
    let _ = config.token_endpoint()?;

    let (epoch, previous, host) = {
        let _guard = session_lock()?;
        require_no_pending_disconnect()?;
        let previous = ChatGptOAuthStore::load_with_scope(backend, scope)?
            .and_then(|record| record.identity)
            .filter(|identity| is_issued_client(&identity.client_id));
        (
            SESSION_EPOCH.load(Ordering::SeqCst),
            previous,
            host_id_with_scope(backend, scope)?,
        )
    };
    let server = LoopbackCallbackServer::bind(&config.callback_ports)?;
    let redirect_uri = server.redirect_uri();
    let pkce = generate_pkce();
    let state = generate_csrf_state();
    let nonce = generate_csrf_state();
    let auth_url = build_authorization_url(
        config,
        &redirect_uri,
        &pkce,
        &state,
        &nonce,
        &host,
        previous.as_ref(),
    )?;

    open_browser(&auth_url)?;
    let callback = server.wait_for_code_with_signals(
        &state,
        callback_timeout,
        cancel,
        Some(sign_in_cancel.signal()),
        Some(epoch),
    )?;
    let _guard = session_lock()?;
    if epoch != SESSION_EPOCH.load(Ordering::SeqCst) {
        return Err(ChatGptOAuthError::Cancelled);
    }
    let record = match exchange_code(
        config,
        callback,
        &redirect_uri,
        &pkce.verifier,
        &nonce,
        previous.as_ref(),
        http_timeout,
    ) {
        Ok(record) => record,
        Err(ExchangeError {
            error,
            verified_identity,
        }) => {
            if let Some(identity) = verified_identity {
                let _ = mark_reauthorization_required_locked(backend, scope, identity);
            }
            return Err(error);
        }
    };
    ChatGptOAuthStore::save_with_scope(backend, scope, &record)?;
    invalidate_session()?;
    Ok(record.status())
}

/// Production entry point for interactive opt-in ChatGPT Plus/Pro OAuth login.
///
/// # Errors
/// Returns a secret-free [`ChatGptOAuthError`].
pub(crate) fn run_oauth_login_flow<F>(
    open_browser: F,
    callback_timeout: Duration,
) -> Result<ChatGptOAuthStatus, ChatGptOAuthError>
where
    F: FnOnce(&str) -> Result<(), ChatGptOAuthError>,
{
    let scope = ensure_desktop_lease()?;
    run_oauth_login_flow_with_scope(
        &KeyringBackend,
        &scope,
        &ChatGptOAuthConfig::default(),
        open_browser,
        callback_timeout,
        DEFAULT_OAUTH_HTTP_TIMEOUT,
        None,
    )
}

#[cfg(test)]
pub(crate) fn fixture_plan_identity() -> PlanIdentity {
    PlanIdentity {
        client_id: "oaiapp_fixture".to_owned(),
        subject: "example-subject".to_owned(),
        id_token: "fixture-id-token".to_owned(),
        models: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Serialize tests that mutate process-global `SESSION_EPOCH` or hold `BROWSER_SIGN_IN`.
    static TEST_SESSION_FLOW_LOCK: Mutex<()> = Mutex::new(());

    fn lock_session_flow_test() -> std::sync::MutexGuard<'static, ()> {
        TEST_SESSION_FLOW_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[derive(Default)]
    struct MemoryKeyring {
        entries: Mutex<HashMap<(String, String), String>>,
        fail_root_read: AtomicBool,
        fail_delete_key: Mutex<Option<String>>,
    }

    impl CredentialBackend for MemoryKeyring {
        fn set_password(&self, service: &str, key: &str, credential: &str) -> keyring::Result<()> {
            self.entries.lock().expect("lock").insert(
                (service.to_string(), key.to_string()),
                credential.to_string(),
            );
            Ok(())
        }

        fn get_password(&self, service: &str, key: &str) -> keyring::Result<String> {
            if key == CHATGPT_OAUTH_KEYRING_KEY && self.fail_root_read.load(Ordering::SeqCst) {
                return Err(keyring::Error::PlatformFailure(
                    "fixture root read failure".into(),
                ));
            }
            self.entries
                .lock()
                .expect("lock")
                .get(&(service.to_string(), key.to_string()))
                .cloned()
                .ok_or(keyring::Error::NoEntry)
        }

        fn delete_credential(&self, service: &str, key: &str) -> keyring::Result<()> {
            if self.fail_delete_key.lock().unwrap().as_deref() == Some(key) {
                return Err(keyring::Error::PlatformFailure(
                    "fixture delete failure".into(),
                ));
            }
            self.entries
                .lock()
                .expect("lock")
                .remove(&(service.to_string(), key.to_string()))
                .map(|_| ())
                .ok_or(keyring::Error::NoEntry)
        }
    }

    fn fixture_record() -> StoredChatGptOAuthTokens {
        StoredChatGptOAuthTokens {
            access_token: "local-fixture-access".to_owned(),
            refresh_token: Some("local-fixture-refresh".to_owned()),
            expires_at: Some(now_unix_secs() + 300),
            token_type: Some("Bearer".to_owned()),
            scope: Some(DEFAULT_SCOPES.to_owned()),
            connected_at: now_unix_secs(),
            identity: Some(fixture_plan_identity()),
            reauthorization_required: false,
        }
    }

    #[test]
    fn oauth_lease_rejects_a_second_handle_and_can_be_reclaimed() {
        let path = std::env::temp_dir().join(format!("nexora-oauth-lease-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let first = acquire_desktop_lease(&path).unwrap();
        assert!(matches!(
            acquire_desktop_lease(&path),
            Err(ChatGptOAuthError::OtherInstance)
        ));
        drop(first);
        drop(acquire_desktop_lease(&path).unwrap());
        std::fs::remove_file(path.join("chatgpt-oauth.lock")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }

    #[test]
    fn active_keyring_slot_is_untouched_when_manifest_read_fails() {
        let backend = MemoryKeyring::default();
        let tokens = fixture_record();
        ChatGptOAuthStore::save_with(&backend, &tokens).unwrap();
        let before = backend.entries.lock().unwrap().clone();
        backend.fail_root_read.store(true, Ordering::SeqCst);
        let rotated = StoredChatGptOAuthTokens {
            access_token: "fresh-local-fixture-access".to_owned(),
            ..tokens
        };
        assert_eq!(
            ChatGptOAuthStore::save_with(&backend, &rotated),
            Err(ChatGptOAuthError::KeyringUnavailable),
        );
        assert_eq!(*backend.entries.lock().unwrap(), before);
    }

    #[test]
    fn partial_disconnect_blocks_usage_and_a_retry_clears_both_slots() {
        let backend = MemoryKeyring::default();
        ChatGptOAuthStore::save_with(&backend, &fixture_record()).unwrap();
        backend
            .set_password(
                CHATGPT_OAUTH_KEYRING_SERVICE,
                &chunk_entry_key(1, 7),
                "stale-local-fixture",
            )
            .unwrap();
        *backend.fail_delete_key.lock().unwrap() = Some(chunk_entry_key(0, 0));
        assert_eq!(
            ChatGptOAuthStore::clear_with(&backend),
            Err(ChatGptOAuthError::KeyringUnavailable)
        );
        assert_eq!(
            backend
                .get_password(CHATGPT_OAUTH_KEYRING_SERVICE, CHATGPT_OAUTH_KEYRING_KEY)
                .unwrap(),
            DISCONNECT_PENDING,
        );
        assert_eq!(
            resolve_access_token_with(&backend, &ChatGptOAuthConfig::default(), Duration::ZERO),
            Err(ChatGptOAuthError::KeyringUnavailable),
        );
        assert!(matches!(
            backend.get_password(CHATGPT_OAUTH_KEYRING_SERVICE, &chunk_entry_key(1, 7)),
            Err(keyring::Error::NoEntry),
        ));
        *backend.fail_delete_key.lock().unwrap() = None;
        ChatGptOAuthStore::clear_with(&backend).unwrap();
        assert!(backend.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn pkce_s256_matches_rfc7636_vector_and_redacts_verifier_in_debug() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(compute_s256_challenge(verifier), expected);

        let pair = generate_pkce();
        assert_eq!(pair.verifier.len(), 43);
        assert_eq!(compute_s256_challenge(&pair.verifier), pair.challenge);
        let debug = format!("{pair:?}");
        assert!(!debug.contains(&pair.verifier));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn constant_time_eq_handles_equal_different_and_length_mismatch() {
        assert!(constant_time_eq(b"abc-123", b"abc-123"));
        assert!(!constant_time_eq(b"abc-123", b"abc-124"));
        assert!(!constant_time_eq(b"abc-123", b"abc-12"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn endpoint_validation_rejects_remote_http_userinfo_and_fragments() {
        assert!(validate_oauth_endpoint_url("https://auth.openai.com/oauth/token").is_ok());
        assert!(validate_oauth_endpoint_url("http://127.0.0.1:8080/oauth/token").is_ok());
        assert!(validate_oauth_endpoint_url("http://auth.openai.com/oauth/token").is_err());
        assert!(
            validate_oauth_endpoint_url("https://user:pass@auth.openai.com/oauth/token").is_err()
        );
        assert!(validate_oauth_endpoint_url("https://auth.openai.com/oauth/token#frag").is_err());
        assert!(validate_oauth_endpoint_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn loopback_callback_verifies_state_and_enforces_timeout() {
        assert_eq!(
            verify_csrf_state(None, "expected-csrf-state"),
            Err(ChatGptOAuthError::StateMismatch)
        );
        assert_eq!(
            verify_csrf_state(Some("tampered-state"), "expected-csrf-state"),
            Err(ChatGptOAuthError::StateMismatch)
        );
        assert_eq!(
            verify_csrf_state(Some("expected-csrf-state"), "expected-csrf-state"),
            Ok(())
        );

        let server = LoopbackCallbackServer::bind(&[]).expect("bind ephemeral loopback");
        let redirect = server.redirect_uri();
        assert!(redirect.starts_with("http://127.0.0.1:"));
        assert!(redirect.ends_with("/auth/callback"));

        // Unauthenticated requests with missing or mismatched CSRF state
        // (including ?error= probes) must receive HTTP 400 without aborting
        // the active sign-in listener, which continues waiting for the valid callback.
        let redirect_for_probes = redirect.clone();
        let handle = std::thread::spawn(move || {
            let unauth_error_no_state =
                reqwest::blocking::get(format!("{redirect_for_probes}?error=access_denied"))
                    .expect("probe 1");
            assert_eq!(unauth_error_no_state.status().as_u16(), 400);

            let unauth_error_bad_state = reqwest::blocking::get(format!(
                "{redirect_for_probes}?error=access_denied&state=tampered-state"
            ))
            .expect("probe 2");
            assert_eq!(unauth_error_bad_state.status().as_u16(), 400);

            let missing_state_code =
                reqwest::blocking::get(format!("{redirect_for_probes}?code=abc")).expect("probe 3");
            assert_eq!(missing_state_code.status().as_u16(), 400);

            let mismatched_state_code = reqwest::blocking::get(format!(
                "{redirect_for_probes}?code=abc&state=tampered-state"
            ))
            .expect("probe 4");
            assert_eq!(mismatched_state_code.status().as_u16(), 400);

            let valid_cb = reqwest::blocking::get(format!(
                "{redirect_for_probes}?code=auth-code-777&state=expected-csrf-state"
            ))
            .expect("valid callback");
            assert_eq!(valid_cb.status().as_u16(), 200);
        });
        let code = server
            .wait_for_code("expected-csrf-state", Duration::from_secs(5), None)
            .expect("valid callback succeeds after ignoring unauthenticated probes");
        assert_eq!(code.code, "auth-code-777");
        handle.join().expect("probe thread joins");

        // Authorization error callback WITH valid CSRF state is rejected cleanly.
        let denied_url = format!("{redirect}?error=access_denied&state=expected-csrf-state");
        let handle = std::thread::spawn(move || {
            let resp = reqwest::blocking::get(denied_url).expect("denied response");
            assert_eq!(resp.status().as_u16(), 400);
        });
        let err = server
            .wait_for_code("expected-csrf-state", Duration::from_secs(5), None)
            .expect_err("authenticated error callback must fail");
        assert_eq!(err, ChatGptOAuthError::AuthorizationDenied);
        let _ = handle.join();

        // Timeout is enforced when no valid callback arrives.
        let timeout_err = server
            .wait_for_code("expected-csrf-state", Duration::from_millis(80), None)
            .expect_err("must time out");
        assert_eq!(timeout_err, ChatGptOAuthError::CallbackTimeout);
    }

    #[test]
    fn expired_token_without_usable_refresh_token_fails_closed() {
        let _flow_guard = lock_session_flow_test();
        let keyring = MemoryKeyring::default();
        let config = ChatGptOAuthConfig {
            issuer: "http://127.0.0.1:9".to_string(),
            client_id: "nexora-test-client".to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };

        // 1. Expired access token with refresh_token = None must fail closed.
        ChatGptOAuthStore::save_with(
            &keyring,
            &StoredChatGptOAuthTokens {
                access_token: "expired-access-secret".to_string(),
                refresh_token: None,
                expires_at: Some(now_unix_secs() - 10),
                token_type: Some("Bearer".to_string()),
                scope: Some(DEFAULT_SCOPES.to_string()),
                connected_at: 1_700_000_000,
                identity: Some(fixture_plan_identity()),
                reauthorization_required: false,
            },
        )
        .expect("save expired token without refresh_token");

        let err = resolve_access_token_with(&keyring, &config, Duration::from_secs(1))
            .expect_err("expired token without refresh_token must fail closed");
        assert_eq!(err, ChatGptOAuthError::TokenRefreshFailed);

        // 2. Expired access token with whitespace-only refresh_token must also fail closed.
        ChatGptOAuthStore::save_with(
            &keyring,
            &StoredChatGptOAuthTokens {
                access_token: "expired-access-secret".to_string(),
                refresh_token: Some("   \t\n  ".to_string()),
                expires_at: Some(now_unix_secs() + 10), // within 60s REFRESH_SKEW_SECS
                token_type: Some("Bearer".to_string()),
                scope: Some(DEFAULT_SCOPES.to_string()),
                connected_at: 1_700_000_000,
                identity: Some(fixture_plan_identity()),
                reauthorization_required: false,
            },
        )
        .expect("save near-expiry token with whitespace refresh_token");

        let err = resolve_access_token_with(&keyring, &config, Duration::from_secs(1))
            .expect_err("near-expiry token with whitespace refresh_token must fail closed");
        assert_eq!(err, ChatGptOAuthError::TokenRefreshFailed);
    }

    fn spawn_rotating_token_server(
        expected_requests: usize,
    ) -> (
        u16,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock rotating token server");
        let port = listener.local_addr().expect("addr").port();
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let refresh_calls_srv = Arc::clone(&refresh_calls);
        let handle = std::thread::spawn(move || {
            for idx in 0..expected_requests {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                assert_eq!(refresh_calls_srv.fetch_add(1, Ordering::SeqCst), idx);

                let expected_refresh = format!("rotating-rt-{}", idx + 1);
                let next_refresh = format!("rotating-rt-{}", idx + 2);
                let next_access = format!("rotated-access-{}", idx + 2);
                let (status, body) = if req.contains(&format!("refresh_token={expected_refresh}")) {
                    (
                        "200 OK",
                        format!(
                            r#"{{"access_token":"{next_access}","refresh_token":"{next_refresh}","expires_in":3600,"token_type":"Bearer"}}"#
                        ),
                    )
                } else {
                    (
                        "400 Bad Request",
                        r#"{"error":"invalid_grant"}"#.to_string(),
                    )
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (port, refresh_calls, handle)
    }

    #[test]
    fn concurrent_callers_near_expiry_single_flight_refresh_without_rotating_token_race() {
        use std::sync::{Arc, Barrier};

        let (port, refresh_calls, server_thread) = spawn_rotating_token_server(3);
        let keyring = Arc::new(MemoryKeyring::default());
        ChatGptOAuthStore::save_with(
            keyring.as_ref(),
            &StoredChatGptOAuthTokens {
                access_token: "initial-near-expiry-access".to_string(),
                refresh_token: Some("rotating-rt-1".to_string()),
                expires_at: Some(now_unix_secs() + 5),
                token_type: Some("Bearer".to_string()),
                scope: Some(DEFAULT_SCOPES.to_string()),
                connected_at: 1_700_000_000,
                identity: Some(fixture_plan_identity()),
                reauthorization_required: false,
            },
        )
        .expect("seed near-expiry token");

        let config = Arc::new(ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port}"),
            client_id: "nexora-test-client".to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        });

        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (kr, cfg, bar) = (
                    Arc::clone(&keyring),
                    Arc::clone(&config),
                    Arc::clone(&barrier),
                );
                std::thread::spawn(move || {
                    bar.wait();
                    resolve_access_token_with(kr.as_ref(), cfg.as_ref(), Duration::from_secs(5))
                })
            })
            .collect();
        for handle in handles {
            let resolved = handle.join().expect("join").expect("ok").expect("some");
            assert_eq!(resolved, "rotated-access-2");
        }
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);

        let barrier2 = Arc::new(Barrier::new(2));
        let refresh_handles: Vec<_> = (0..2)
            .map(|_| {
                let (kr, cfg, bar) = (
                    Arc::clone(&keyring),
                    Arc::clone(&config),
                    Arc::clone(&barrier2),
                );
                std::thread::spawn(move || {
                    bar.wait();
                    refresh_tokens_with(kr.as_ref(), cfg.as_ref(), Duration::from_secs(5))
                })
            })
            .collect();
        for handle in refresh_handles {
            let status = handle.join().expect("join").expect("refresh succeeds");
            assert!(status.connected && status.has_refresh_token);
        }
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 3);

        let final_record = ChatGptOAuthStore::load_with(keyring.as_ref())
            .expect("load final")
            .expect("present");
        assert_eq!(final_record.access_token, "rotated-access-4");
        assert_eq!(final_record.refresh_token.as_deref(), Some("rotating-rt-4"));
        server_thread.join().expect("mock rotating server exits");
    }

    // This synthetic key is used only to sign local OAuth fixtures.
    const FIXTURE_PRIVATE_KEY: &str = concat!(
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC64InurQdNqO_fnw4hNutQ1tZjDiu6bXeOXFxpDycWLpNvFvEUebZn09m4Vp3VjieWIyMa",
        "i7uYz7b_kPh_lECVfUJQPP5hsaPfNY9WyhrL4C3aPdoiO2EKK-3_WxzeHJL4yb5K_jA_s9UqvBK9T-0KBKxDWpRc0vWAa34TVHZviGJWps-r1Sk0qiAD7-d4",
        "ziG2-RZNwieEUGUBkY7Tv32o2CbkPN_fo1tIMTcbtO4BvrlbEcm6E1HypwADX3R9k798vxxIxK9ZKUU7XwEQMEEfS34wcEmu9FI-RqniSLMscb0gme278pPJ",
        "GWRCWtJVnNIPvT6DtxzC7g0S8aA1z1yxAgMBAAECggEAE1SzR_JzmTOyvn_SQxrOfDIBAsWePd5heeMxdvenjCqcuybVINBXT0WxlZk2Ifdevm_bi30tVmkU",
        "LbsWDYJtLNmEcM7iZIIu6KG-dlSZMYI61ALGa_iW-Omi6KGI6qvrgxtBWToSjSBh2lYSnS0loL3rgNsfkjuLRDG6XK8FwlMc0rsh9PXQPaWMGqWPqtt04ak6",
        "nlgDqTtZauWsNVVbqKPadMljkRzLpGjGyunMnneG4EscuLRbpAGBOyUr6XTaprsJNBBb6aKs9D1HzWrSSfirHVmokMFJPaHQsAOeFE7N9CVvKdtAWoHvCdUg",
        "E3_bOjrurpEo8omskj5buH-BbQKBgQDa5yY0Me6WzY1c0GVNbodYsuArq43fY-yvqI0YWLmYWaR1B0ZNHy1HWL7uf8QGLDdGO66HWI9aoxv8_zb6Mzsw8q4e",
        "uvj7EZvMfL4AX5wabf2OeKVxLCf_lcc_7GtO_njzqn07EisKY7yTLRYRRrWq_bneL-X6ALUEsi801CXVjQKBgQDai_x6u5ZgRlyP1ljndLbazC-jjYPRgYEX",
        "XCEhPcxkvQE4yr1BcKpP-DquM5AGdW2Rz65xhk1qMrv9bmMZ-3gme20pR2K1AbLQr9E20IVcAuaiaIwdx5jhoRqHgaPM4NsuTrOylg1Gla2R1KREkfM38mQD",
        "Z2SpvfENvko0XgTgtQKBgQCY9xV3O70hnBnAVc-0QBxMGxMS8Rp-k_D8_qc58CYhE-Mb9C1oDvUw-g7R0m5EcfleFq5q3bOu_nO8Ywsj41Dlb2_FAZqitzdL",
        "uY9761PGFD4upJfXcaexrhRB0w9Zb_FNNEG2WYfCBtfrcIIPKH4iMpMDdC0cegoEcg8St3yIsQKBgA2Mhi-Zt53qXsywDjzzOG7iH8X0_QodDELFfGxSJ3zL",
        "VusNbqRC9twUWJb6LWCpIQzcIkV5fQ0rbgotMMmolG9WUZMwkMcF_u7_J2pNjScCKwhAkQc7m5fJZBqoh94oxo3dZTiaIuKQXv1egHyby8dZKKgpXc1EMFCY",
        "zoLmnUjNAoGABfkuzhcsISlSA36S_mrdh4U3AOD8CZn0APKyRXjACMLwPZhyfotTsxv2aNI3iW4YeSKQ4hd6rUvOyTb5S9SrHnmeZLswllJkDKhmwjraFfDR",
        "zjvnUfw2Uyn0kIbPMxQexzLzoYhme7mzZGSWVVpRwdczx_O_sn1IaaNMPCwHXKg",
    );
    const FIXTURE_MODULUS: &str = concat!(
        "uuCJ7q0HTajv358OITbrUNbWYw4rum13jlxcaQ8nFi6TbxbxFHm2Z9PZuFad1Y4nliMjGou7mM-2_5D4f5RAlX1CUDz-YbGj3zWPVsoay-At2j3aIjthCivt",
        "_1sc3hyS-Mm-Sv4wP7PVKrwSvU_tCgSsQ1qUXNL1gGt-E1R2b4hiVqbPq9UpNKogA-_neM4htvkWTcInhFBlAZGO0799qNgm5Dzf36NbSDE3G7TuAb65WxHJ",
        "uhNR8qcAA190fZO_fL8cSMSvWSlFO18BEDBBH0t-MHBJrvRSPkap4kizLHG9IJntu_KTyRlkQlrSVZzSD70-g7ccwu4NEvGgNc9csQ",
    );

    fn signed_fixture_id_token(port: u16, nonce: &str) -> String {
        let key = ring::signature::RsaKeyPair::from_pkcs8(
            &URL_SAFE_NO_PAD.decode(FIXTURE_PRIVATE_KEY).unwrap(),
        )
        .unwrap();
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","kid":"fixture"}"#);
        let claims = serde_json::json!({
            "iss": format!("http://127.0.0.1:{port}"),
            "sub": "example-subject",
            "aud": ["oaiapp_fixture"],
            "nonce": nonce,
            "iat": now_unix_secs(),
            "exp": now_unix_secs() + 600,
        });
        let signed = format!("{header}.{}", URL_SAFE_NO_PAD.encode(claims.to_string()));
        let mut signature = vec![0u8; key.public().modulus_len()];
        key.sign(
            &ring::signature::RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            signed.as_bytes(),
            &mut signature,
        )
        .unwrap();
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    fn oauth_fixture() -> (
        u16,
        std::sync::mpsc::Sender<String>,
        std::thread::JoinHandle<()>,
    ) {
        let token_listener = TcpListener::bind("127.0.0.1:0").expect("bind mock token server");
        let port = token_listener.local_addr().expect("addr").port();
        let (send_nonce, receive_nonce) = std::sync::mpsc::channel::<String>();
        let server_thread = std::thread::spawn(move || {
            for idx in 0..4 {
                let (mut stream, _) = token_listener.accept().expect("accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut req = String::new();
                let mut buf = [0u8; 4096];
                let request_body;
                loop {
                    let n = stream.read(&mut buf).expect("read fixture request");
                    assert!(n > 0, "client closed fixture request");
                    req.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if let Some((headers, body)) = req.split_once("\r\n\r\n") {
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, val) = line.split_once(':')?;
                                if name.eq_ignore_ascii_case("content-length") {
                                    val.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if body.len() >= content_length {
                            request_body = body;
                            break;
                        }
                    }
                }
                let body = match idx {
                    0 => {
                        assert!(req.starts_with("POST /api/accounts/oauth/token "));
                        assert!(request_body.contains("grant_type=authorization_code"));
                        assert!(request_body.contains("code=live-code-42"));
                        assert!(request_body.contains("client_id=oaiapp_fixture"));
                        assert!(request_body.contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1"));
                        assert!(request_body.contains("code_verifier="));
                        let nonce = receive_nonce.recv_timeout(Duration::from_secs(5)).unwrap();
                        serde_json::json!({
                            "access_token": "chatgpt-initial-access-secret",
                            "refresh_token": "chatgpt-refresh-secret-1",
                            "expires_in": 10,
                            "token_type": "Bearer",
                            "id_token": signed_fixture_id_token(port, &nonce),
                            "scope": DEFAULT_SCOPES,
                        })
                    }
                    1 => {
                        assert!(req.starts_with("GET /.well-known/jwks.json "));
                        serde_json::json!({"keys": [{
                            "kid": "fixture", "kty": "RSA", "alg": "RS256",
                            "n": FIXTURE_MODULUS, "e": "AQAB",
                        }]})
                    }
                    2 => {
                        assert!(req.starts_with("GET /v1/models "));
                        assert!(req.to_ascii_lowercase().contains("authorization: bearer chatgpt-initial-access-secret"));
                        serde_json::json!({"models": [
                            {"slug":"fixture-model","display_name":"Fixture model","visibility":"list"},
                            {"slug":"hidden-model","display_name":"Hidden fixture","visibility":"hidden"},
                        ]})
                    }
                    _ => {
                        assert!(request_body.contains("grant_type=refresh_token"));
                        assert!(request_body.contains("client_id=oaiapp_fixture"));
                        assert!(request_body.contains("refresh_token=chatgpt-refresh-secret-1"));
                        serde_json::json!({
                            "access_token": "chatgpt-refreshed-access-secret",
                            "expires_in": 3600, "token_type": "Bearer",
                        })
                    }
                }.to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });
        (port, send_nonce, server_thread)
    }

    #[test]
    fn full_oauth_flow_exchanges_refreshes_and_keeps_tokens_in_keyring_only() {
        let _flow_guard = lock_session_flow_test();
        let (port, send_nonce, server_thread) = oauth_fixture();
        let keyring = MemoryKeyring::default();
        let config = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port}"),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(), // Ephemeral port for deterministic test.
        };

        let status = run_oauth_login_flow_with(
            &keyring,
            &config,
            |auth_url| {
                let parsed = Url::parse(auth_url).expect("valid auth url");
                assert_eq!(parsed.path(), "/api/accounts/authorize");
                let query: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
                assert_eq!(query["code_challenge_method"], "S256");
                assert_eq!(query["agent_name_hint"], "Nexora");
                assert_eq!(query["client_id"], DYNAMIC_CLIENT_ID);
                assert!(query["ext_agent_host_id"].starts_with("urn:uuid:"));
                assert!(!query.contains_key("codex_cli_simplified_flow"));
                send_nonce.send(query["nonce"].clone()).unwrap();
                let cb_url = format!(
                    "{}?code=live-code-42&client_id=oaiapp_fixture&state={}",
                    query["redirect_uri"], query["state"]
                );
                std::thread::spawn(move || {
                    let _ = reqwest::blocking::get(cb_url);
                });
                Ok(())
            },
            Duration::from_secs(5),
            Duration::from_secs(5),
            None,
        )
        .expect("interactive oauth login succeeds");

        assert!(status.connected);
        assert!(status.has_refresh_token);
        assert_eq!(status.auth_mode, "oauth");
        assert!(status.expires_at.is_some());
        assert_eq!(status.models.len(), 1);
        assert_eq!(status.models[0].slug, "fixture-model");

        // Stored record Debug output never leaks secret values.
        let stored = ChatGptOAuthStore::load_with(&keyring)
            .expect("load")
            .expect("present");
        let debug = format!("{stored:?}");
        assert!(!debug.contains("chatgpt-initial-access-secret"));
        assert!(!debug.contains("chatgpt-refresh-secret-1"));
        assert!(debug.contains("[REDACTED]"));

        // Resolving access token triggers automatic refresh because expires_in=10 < REFRESH_SKEW_SECS.
        let fresh_token = resolve_access_token_with(&keyring, &config, Duration::from_secs(5))
            .expect("resolve succeeds")
            .expect("token present");
        assert_eq!(fresh_token, "chatgpt-refreshed-access-secret");

        // Preserved refresh token when server did not rotate it.
        let updated = ChatGptOAuthStore::load_with(&keyring)
            .expect("load updated")
            .expect("present");
        assert_eq!(
            updated.refresh_token.as_deref(),
            Some("chatgpt-refresh-secret-1")
        );

        // Clearing disconnects cleanly.
        ChatGptOAuthStore::clear_with(&keyring).expect("clear");
        assert!(!ChatGptOAuthStore::is_connected_with(&keyring).expect("status"));
        assert!(
            resolve_access_token_with(&keyring, &config, Duration::from_secs(5))
                .expect("resolve disconnected")
                .is_none()
        );

        server_thread.join().expect("mock token server exits");

        // Model a disconnect arriving while the browser is open. Its late
        // callback has the right state but can neither exchange nor save.
        let previous_host = host_id(&keyring).expect("stable host id");
        let late = run_oauth_login_flow_with(
            &keyring,
            &config,
            |auth_url| {
                let parsed = Url::parse(auth_url).unwrap();
                let query: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
                assert_eq!(query["ext_agent_host_id"], previous_host);
                {
                    let _guard = session_lock().unwrap();
                    invalidate_session().unwrap();
                    ChatGptOAuthStore::clear_with(&keyring).unwrap();
                }
                let url = format!(
                    "{}?code=late-fixture-code&state={}&client_id=oaiapp_fixture",
                    query["redirect_uri"], query["state"],
                );
                std::thread::spawn(move || {
                    reqwest::blocking::get(url).expect("complete fixture callback");
                });
                Ok(())
            },
            Duration::from_secs(5),
            Duration::ZERO,
            None,
        );
        assert_eq!(late, Err(ChatGptOAuthError::Cancelled));
        assert!(!ChatGptOAuthStore::is_connected_with(&keyring).unwrap());
    }

    #[test]
    fn errors_stay_secret_free() {
        let all = [
            ChatGptOAuthError::KeyringUnavailable,
            ChatGptOAuthError::InvalidEndpoint,
            ChatGptOAuthError::LoopbackBindFailed,
            ChatGptOAuthError::BrowserOpenFailed,
            ChatGptOAuthError::CallbackTimeout,
            ChatGptOAuthError::StateMismatch,
            ChatGptOAuthError::AuthorizationDenied,
            ChatGptOAuthError::MissingCode,
            ChatGptOAuthError::TokenExchangeFailed,
            ChatGptOAuthError::CallbackClientIdInvalid,
            ChatGptOAuthError::TokenEndpointRejected { status: 400 },
            ChatGptOAuthError::IdTokenVerificationFailed,
            ChatGptOAuthError::JwksFetchFailed,
            ChatGptOAuthError::TokenRefreshFailed,
            ChatGptOAuthError::TokenRefreshTransient,
            ChatGptOAuthError::ReauthorizationRequired,
            ChatGptOAuthError::ResponseTooLarge,
            ChatGptOAuthError::NotConnected,
            ChatGptOAuthError::Cancelled,
            ChatGptOAuthError::RevocationUnconfirmed,
            ChatGptOAuthError::OtherInstance,
        ];
        for err in all {
            let text = err.to_string().to_lowercase();
            assert!(!text.contains("sk-"));
            assert!(!text.contains("bearer "));
            assert!(!text.contains("eyj"));
        }
    }

    struct SizeLimitedKeyring {
        max_utf16_chars: usize,
        entries: Mutex<HashMap<(String, String), String>>,
        fail_after_writes: Mutex<Option<usize>>,
    }

    impl SizeLimitedKeyring {
        fn new(max_utf16_chars: usize) -> Self {
            Self {
                max_utf16_chars,
                entries: Mutex::new(HashMap::new()),
                fail_after_writes: Mutex::new(None),
            }
        }

        fn arm_fail_after_writes(&self, remaining_ok_writes: usize) {
            *self.fail_after_writes.lock().expect("lock") = Some(remaining_ok_writes);
        }
    }

    impl CredentialBackend for SizeLimitedKeyring {
        fn set_password(&self, service: &str, key: &str, credential: &str) -> keyring::Result<()> {
            if credential.encode_utf16().count() > self.max_utf16_chars {
                return Err(keyring::Error::TooLong(
                    "password".to_string(),
                    u32::try_from(self.max_utf16_chars).unwrap_or(1280),
                ));
            }
            let mut countdown = self.fail_after_writes.lock().expect("lock");
            if let Some(rem) = *countdown {
                if rem == 0 {
                    *countdown = None;
                    return Err(keyring::Error::PlatformFailure(
                        "simulated mid-chunk keyring failure".into(),
                    ));
                }
                *countdown = Some(rem - 1);
            }
            drop(countdown);

            self.entries.lock().expect("lock").insert(
                (service.to_string(), key.to_string()),
                credential.to_string(),
            );
            Ok(())
        }

        fn get_password(&self, service: &str, key: &str) -> keyring::Result<String> {
            self.entries
                .lock()
                .expect("lock")
                .get(&(service.to_string(), key.to_string()))
                .cloned()
                .ok_or(keyring::Error::NoEntry)
        }

        fn delete_credential(&self, service: &str, key: &str) -> keyring::Result<()> {
            self.entries
                .lock()
                .expect("lock")
                .remove(&(service.to_string(), key.to_string()))
                .map(|_| ())
                .ok_or(keyring::Error::NoEntry)
        }
    }

    #[test]
    fn chunked_keyring_store_handles_3kb_access_token_under_windows_1280_char_limit_and_preserves_previous_on_failure(
    ) {
        let keyring = SizeLimitedKeyring::new(1280);
        let large_access_token = format!("eyJhbGciOiJSUzI1NiJ9.{}.sig", "A".repeat(3072));
        let initial_refresh_token = format!("rt_initial_{}", "R".repeat(600));

        let initial = StoredChatGptOAuthTokens {
            access_token: large_access_token.clone(),
            refresh_token: Some(initial_refresh_token.clone()),
            expires_at: Some(1_900_000_000),
            token_type: Some("Bearer".to_string()),
            scope: Some(DEFAULT_SCOPES.to_string()),
            connected_at: 1_899_996_400,
            identity: Some(fixture_plan_identity()),
            reauthorization_required: false,
        };

        ChatGptOAuthStore::save_with(&keyring, &initial)
            .expect("3 KB access token chunks and saves under 1280-char limit");

        {
            let snapshot = keyring.entries.lock().expect("lock");
            assert!(snapshot.len() >= 4, "expected manifest + multiple chunks");
            for ((service, _key), value) in snapshot.iter() {
                assert_eq!(service, CHATGPT_OAUTH_KEYRING_SERVICE);
                assert!(
                    value.chars().count() <= MAX_KEYRING_CHUNK_CHARS,
                    "every keyring entry must be <= {MAX_KEYRING_CHUNK_CHARS} chars, got {}",
                    value.chars().count()
                );
            }
        }

        let loaded = ChatGptOAuthStore::load_with(&keyring)
            .expect("load chunked tokens")
            .expect("tokens present");
        assert_eq!(loaded.access_token, large_access_token);
        assert_eq!(
            loaded.refresh_token.as_deref(),
            Some(initial_refresh_token.as_str())
        );

        // Simulate a write failure after the first chunk of a rotated update:
        // the previously saved tokens (including the existing refresh_token)
        // must remain intact and recoverable.
        keyring.arm_fail_after_writes(1);
        let failed_update = StoredChatGptOAuthTokens {
            access_token: format!("eyJ.rotated.{}", "B".repeat(3072)),
            refresh_token: Some("rt_rotated_lost_if_not_transactional".to_string()),
            expires_at: Some(1_900_003_600),
            token_type: Some("Bearer".to_string()),
            scope: Some(DEFAULT_SCOPES.to_string()),
            connected_at: 1_899_996_400,
            identity: Some(fixture_plan_identity()),
            reauthorization_required: false,
        };
        let err = ChatGptOAuthStore::save_with(&keyring, &failed_update)
            .expect_err("mid-save failure must surface error");
        assert_eq!(err, ChatGptOAuthError::KeyringUnavailable);

        let preserved = ChatGptOAuthStore::load_with(&keyring)
            .expect("previous tokens still load cleanly after failed update")
            .expect("previous tokens intact");
        assert_eq!(preserved.access_token, large_access_token);
        assert_eq!(
            preserved.refresh_token.as_deref(),
            Some(initial_refresh_token.as_str())
        );

        // Now perform a successful update to a smaller token and verify stale chunks from the old slot are deleted.
        let compact_update = StoredChatGptOAuthTokens {
            access_token: "compact-access-token".to_string(),
            refresh_token: Some("compact-refresh-token".to_string()),
            expires_at: Some(1_900_007_200),
            token_type: Some("Bearer".to_string()),
            scope: Some(DEFAULT_SCOPES.to_string()),
            connected_at: 1_899_996_400,
            identity: Some(fixture_plan_identity()),
            reauthorization_required: false,
        };
        ChatGptOAuthStore::save_with(&keyring, &compact_update).expect("compact update succeeds");
        {
            let snapshot = keyring.entries.lock().expect("lock");
            assert_eq!(
                snapshot.len(),
                2,
                "stale chunks from previous slot must be cleaned up, leaving only manifest + 1 chunk"
            );
        }

        ChatGptOAuthStore::clear_with(&keyring).expect("clear chunked store");
        assert!(
            keyring.entries.lock().expect("lock").is_empty(),
            "clear_with must remove manifest and all chunk entries"
        );
    }

    #[test]
    fn distinct_app_data_roots_isolate_leases_and_keyring_scopes() {
        let dir_a = std::env::temp_dir().join(format!("nexora-oauth-scope-a-{}", Uuid::new_v4()));
        let dir_b = std::env::temp_dir().join(format!("nexora-oauth-scope-b-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir_a).unwrap();
        std::fs::create_dir(&dir_b).unwrap();

        let lease_a = acquire_desktop_lease(&dir_a).unwrap();
        let lease_b = acquire_desktop_lease(&dir_b).unwrap();
        let scope_a = KeyringScope::for_directory(&dir_a).unwrap();
        let scope_b = KeyringScope::for_directory(&dir_b).unwrap();
        assert_ne!(scope_a, scope_b);
        assert_eq!(
            scope_a,
            KeyringScope::for_directory(&dir_a.join(".")).unwrap()
        );

        let shared_keyring = MemoryKeyring::default();
        let mut record_a = fixture_record();
        record_a.access_token = "scope-a-access".to_owned();
        let mut record_b = fixture_record();
        record_b.access_token = "scope-b-access".to_owned();

        ChatGptOAuthStore::save_with_scope(&shared_keyring, &scope_a, &record_a).unwrap();
        ChatGptOAuthStore::save_with_scope(&shared_keyring, &scope_b, &record_b).unwrap();
        let host_a = host_id_with_scope(&shared_keyring, &scope_a).unwrap();
        let host_b = host_id_with_scope(&shared_keyring, &scope_b).unwrap();
        assert_ne!(host_a, host_b);

        // Disconnecting root A must not affect root B's tokens on the same OS keyring.
        ChatGptOAuthStore::clear_with_scope(&shared_keyring, &scope_a).unwrap();
        assert!(
            ChatGptOAuthStore::load_with_scope(&shared_keyring, &scope_a)
                .unwrap()
                .is_none()
        );
        let loaded_b = ChatGptOAuthStore::load_with_scope(&shared_keyring, &scope_b)
            .unwrap()
            .expect("root B grant remains intact");
        assert_eq!(loaded_b.access_token, "scope-b-access");

        drop(lease_a);
        drop(lease_b);
        let _ = std::fs::remove_file(dir_a.join("chatgpt-oauth.lock"));
        let _ = std::fs::remove_file(dir_b.join("chatgpt-oauth.lock"));
        let _ = std::fs::remove_dir(&dir_a);
        let _ = std::fs::remove_dir(&dir_b);
    }

    #[test]
    fn cleanup_pending_recovers_stale_slot_without_losing_active_rotated_grant() {
        let backend = MemoryKeyring::default();
        let initial = fixture_record();
        ChatGptOAuthStore::save_with(&backend, &initial).unwrap();

        // Fail stale-slot (slot 0) deletion after slot 1 + cleanup_pending manifest are committed.
        *backend.fail_delete_key.lock().unwrap() = Some(chunk_entry_key(0, 0));
        let mut rotated = fixture_record();
        rotated.access_token = "rotated-active-access".to_owned();
        rotated.refresh_token = Some("rotated-active-refresh".to_owned());
        assert_eq!(
            ChatGptOAuthStore::save_with(&backend, &rotated),
            Err(ChatGptOAuthError::KeyringUnavailable)
        );

        let raw_manifest = backend
            .get_password(CHATGPT_OAUTH_KEYRING_SERVICE, CHATGPT_OAUTH_KEYRING_KEY)
            .unwrap();
        let manifest: KeyringChunkManifest = serde_json::from_str(&raw_manifest).unwrap();
        assert_eq!(manifest.slot, 1);
        assert!(manifest.cleanup_pending);

        // Once the keyring delete error clears, loading finishes stale-slot deletion
        // and recovers the active rotated grant without losing it.
        *backend.fail_delete_key.lock().unwrap() = None;
        let recovered = ChatGptOAuthStore::load_with(&backend)
            .expect("recovery load succeeds")
            .expect("active rotated grant preserved");
        assert_eq!(recovered.access_token, "rotated-active-access");
        assert_eq!(
            recovered.refresh_token.as_deref(),
            Some("rotated-active-refresh")
        );
        assert!(matches!(
            backend.get_password(CHATGPT_OAUTH_KEYRING_SERVICE, &chunk_entry_key(0, 0)),
            Err(keyring::Error::NoEntry)
        ));
        let clean_manifest: KeyringChunkManifest = serde_json::from_str(
            &backend
                .get_password(CHATGPT_OAUTH_KEYRING_SERVICE, CHATGPT_OAUTH_KEYRING_KEY)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(clean_manifest.slot, 1);
        assert!(!clean_manifest.cleanup_pending);

        // Also verify a direct save recovers from cleanup_pending without discarding the grant.
        *backend.fail_delete_key.lock().unwrap() = Some(chunk_entry_key(1, 0));
        let mut rotated_again = fixture_record();
        rotated_again.access_token = "second-rotated-access".to_owned();
        assert_eq!(
            ChatGptOAuthStore::save_with(&backend, &rotated_again),
            Err(ChatGptOAuthError::KeyringUnavailable)
        );
        *backend.fail_delete_key.lock().unwrap() = None;
        let mut third = fixture_record();
        third.access_token = "third-rotated-access".to_owned();
        ChatGptOAuthStore::save_with(&backend, &third).expect("save recovers from cleanup_pending");
        let final_loaded = ChatGptOAuthStore::load_with(&backend).unwrap().unwrap();
        assert_eq!(final_loaded.access_token, "third-rotated-access");
    }

    fn spawn_scripted_token_server(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted token server");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (port, handle)
    }

    #[test]
    fn refresh_distinguishes_terminal_and_transient_errors_and_preserves_verified_client_id() {
        let _flow_guard = lock_session_flow_test();
        let (port, server) = spawn_scripted_token_server(vec![
            (
                "503 Service Unavailable",
                r#"{"error":"temporarily_unavailable","error_description":"upstream busy"}"#,
            ),
            (
                "400 Bad Request",
                r#"{"error":"invalid_grant","error_description":"refresh token revoked"}"#,
            ),
        ]);
        let keyring = MemoryKeyring::default();
        let config = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port}"),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };
        let mut seeded = fixture_record();
        seeded.expires_at = Some(now_unix_secs() + 10); // triggers refresh
        ChatGptOAuthStore::save_with(&keyring, &seeded).unwrap();

        // 1. Transient error (503 / temporarily_unavailable) preserves grant for retry.
        let transient = refresh_tokens_with(&keyring, &config, Duration::from_secs(5));
        assert_eq!(transient, Err(ChatGptOAuthError::TokenRefreshTransient));
        let after_transient = ChatGptOAuthStore::status_with(&keyring).unwrap();
        assert!(after_transient.connected);
        assert!(after_transient.has_refresh_token);

        // 2. Terminal error (400 / invalid_grant) marks grant unusable and NOT connected,
        //    while preserving verified client_id / id_token_hint for reauthorization.
        let terminal = refresh_tokens_with(&keyring, &config, Duration::from_secs(5));
        assert_eq!(terminal, Err(ChatGptOAuthError::ReauthorizationRequired));
        let after_terminal = ChatGptOAuthStore::status_with(&keyring).unwrap();
        assert!(!after_terminal.connected);
        assert!(!after_terminal.has_refresh_token);
        assert_eq!(
            after_terminal.problem,
            Some(ChatGptOAuthProblem::ReauthorizationRequired)
        );
        assert!(session_reference_with(&keyring).unwrap().is_none());
        assert_eq!(
            resolve_access_token_with(&keyring, &config, Duration::from_secs(5)),
            Err(ChatGptOAuthError::ReauthorizationRequired)
        );

        // 3. Subsequent authorization attempt automatically reuses the saved verified client_id.
        let cancel = AtomicBool::new(true);
        let mut captured_auth_url = String::new();
        let err = run_oauth_login_flow_with(
            &keyring,
            &config,
            |auth_url| {
                captured_auth_url = auth_url.to_owned();
                Ok(())
            },
            Duration::from_secs(5),
            Duration::from_secs(5),
            Some(&cancel),
        )
        .expect_err("pre-cancelled flow exits after generating auth url");
        assert_eq!(err, ChatGptOAuthError::Cancelled);
        let parsed = Url::parse(&captured_auth_url).expect("valid reauth url");
        let query: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], "oaiapp_fixture");
        assert_eq!(query["id_token_hint"], "fixture-id-token");
        assert!(!query.contains_key("agent_name_hint"));

        server.join().unwrap();
    }

    #[test]
    fn invalidating_session_promptly_cancels_waiting_callback_listener_and_releases_lock() {
        let _flow_guard = lock_session_flow_test();
        let keyring = Arc::new(MemoryKeyring::default());
        let config = ChatGptOAuthConfig {
            issuer: "https://auth.openai.com".to_string(),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };
        let (opened_tx, opened_rx) = std::sync::mpsc::channel::<u16>();
        let keyring_bg = Arc::clone(&keyring);
        let config_bg = config.clone();
        let started = Instant::now();
        let handle = std::thread::spawn(move || {
            run_oauth_login_flow_with(
                keyring_bg.as_ref(),
                &config_bg,
                |auth_url| {
                    let parsed = Url::parse(auth_url).unwrap();
                    let redirect = parsed
                        .query_pairs()
                        .find(|(k, _)| k == "redirect_uri")
                        .map(|(_, v)| v.into_owned())
                        .unwrap();
                    let port = Url::parse(&redirect).unwrap().port().unwrap();
                    opened_tx.send(port).unwrap();
                    Ok(())
                },
                Duration::from_secs(30),
                Duration::from_secs(5),
                None,
            )
        });

        let bound_port = opened_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Simulate disconnect / session invalidation while browser wait is active.
        invalidate_session().unwrap();
        let outcome = handle.join().expect("waiting sign-in thread exits");
        assert_eq!(outcome, Err(ChatGptOAuthError::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "cancellation must release listener promptly, not wait for 30s timeout"
        );
        // Both the loopback port and BROWSER_SIGN_IN lock are immediately free again.
        let rebound = TcpListener::bind(("127.0.0.1", bound_port));
        assert!(
            rebound.is_ok(),
            "callback port must be released immediately"
        );
        assert!(
            BROWSER_SIGN_IN.try_lock().is_ok(),
            "sign-in lock must be released immediately"
        );
    }

    fn spawn_owned_scripted_token_server(
        responses: Vec<(&'static str, String)>,
    ) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted token server");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (port, handle)
    }

    #[test]
    fn token_endpoint_accepts_large_24kb_jwt_payload_and_rejects_over_limit() {
        let _flow_guard = lock_session_flow_test();

        // Construct a realistic ~24 KB OAuth token response containing large
        // access_token, id_token, and refresh_token JWT strings.
        let large_access = format!("eyJhbGciOiJSUzI1NiJ9.{}.sig", "A".repeat(10_500));
        let large_id = format!("eyJhbGciOiJSUzI1NiJ9.{}.sig", "I".repeat(10_000));
        let large_refresh = format!("rt_{}", "R".repeat(4_000));
        let large_body = serde_json::json!({
            "access_token": large_access,
            "id_token": large_id,
            "refresh_token": large_refresh,
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": DEFAULT_SCOPES,
        })
        .to_string();
        assert!(
            large_body.len() > 24 * 1024 && large_body.len() < MAX_TOKEN_RESPONSE_BYTES,
            "expected ~24 KB valid token payload, got {} bytes",
            large_body.len()
        );

        // Over-limit payload exceeds MAX_TOKEN_RESPONSE_BYTES (128 KiB).
        let oversized_body = format!(
            r#"{{"access_token":"{}"}}"#,
            "X".repeat(MAX_TOKEN_RESPONSE_BYTES + 1024)
        );
        assert!(oversized_body.len() > MAX_TOKEN_RESPONSE_BYTES);

        let (port, server) = spawn_owned_scripted_token_server(vec![
            ("200 OK", large_body),
            ("200 OK", oversized_body),
        ]);
        let token_url = Url::parse(&format!("http://127.0.0.1:{port}/api/accounts/oauth/token"))
            .expect("valid loopback token url");

        // 1. ~24 KB token response parses cleanly and round-trips through the chunked keyring store.
        let parsed = post_token_form(
            token_url.clone(),
            &[("grant_type", "refresh_token")],
            Duration::from_secs(5),
        )
        .expect("24 KB OAuth token response must succeed");
        assert_eq!(parsed.access_token, large_access);
        assert_eq!(parsed.id_token.as_deref(), Some(large_id.as_str()));
        assert_eq!(
            parsed.refresh_token.as_deref(),
            Some(large_refresh.as_str())
        );

        let keyring = SizeLimitedKeyring::new(1280);
        let stored = StoredChatGptOAuthTokens {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            expires_at: Some(1_900_000_000),
            token_type: parsed.token_type,
            scope: parsed.scope,
            connected_at: 1_899_996_400,
            identity: Some(PlanIdentity {
                client_id: "oaiapp_large_jwt_fixture".to_string(),
                subject: "sub-large-jwt".to_string(),
                id_token: large_id.clone(),
                models: vec![PlanModel {
                    slug: "gpt-5.4".to_string(),
                    display_name: "GPT-5.4".to_string(),
                }],
            }),
            reauthorization_required: false,
        };
        ChatGptOAuthStore::save_with(&keyring, &stored)
            .expect("24 KB token bundle saves under 1280-char keyring chunk limit");
        let loaded = ChatGptOAuthStore::load_with(&keyring)
            .expect("load 24 KB token bundle")
            .expect("bundle present");
        assert_eq!(loaded.access_token, large_access);
        assert_eq!(
            loaded.identity.as_ref().map(|id| id.id_token.as_str()),
            Some(large_id.as_str())
        );

        // 2. Over-limit (> MAX_TOKEN_RESPONSE_BYTES) response is rejected with ResponseTooLarge.
        match post_token_form(
            token_url,
            &[("grant_type", "refresh_token")],
            Duration::from_secs(5),
        ) {
            Err(err) => assert_eq!(err, TokenEndpointError::ResponseTooLarge),
            Ok(_) => panic!("over-limit OAuth token response must be rejected"),
        }

        server.join().unwrap();
    }

    fn signed_fixture_id_token_with(
        port: u16,
        client_id: &str,
        nonce: &str,
        iat_offset_secs: i64,
    ) -> String {
        let key = ring::signature::RsaKeyPair::from_pkcs8(
            &URL_SAFE_NO_PAD.decode(FIXTURE_PRIVATE_KEY).unwrap(),
        )
        .unwrap();
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","kid":"fixture"}"#);
        let now = now_unix_secs();
        let claims = serde_json::json!({
            "iss": format!("http://127.0.0.1:{port}"),
            "sub": "example-subject",
            "aud": [client_id],
            "nonce": nonce,
            "iat": now + iat_offset_secs,
            "exp": now + 600,
        });
        let signed = format!("{header}.{}", URL_SAFE_NO_PAD.encode(claims.to_string()));
        let mut signature = vec![0u8; key.public().modulus_len()];
        key.sign(
            &ring::signature::RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            signed.as_bytes(),
            &mut signature,
        )
        .unwrap();
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    fn spawn_exchange_server(
        client_id: &'static str,
        scope: Option<&'static str>,
        iat_offset_secs: i64,
        models_status: &'static str,
        models_body: &'static str,
    ) -> (
        u16,
        std::sync::mpsc::Sender<String>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind custom exchange server");
        let port = listener.local_addr().expect("addr").port();
        let (send_nonce, receive_nonce) = std::sync::mpsc::channel::<String>();
        let handle = std::thread::spawn(move || {
            for idx in 0..3 {
                let (mut stream, _) = listener.accept().expect("accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let (status, body) = match idx {
                    0 => {
                        let nonce = receive_nonce.recv_timeout(Duration::from_secs(5)).unwrap();
                        let mut token_payload = serde_json::json!({
                            "access_token": "custom-access-secret",
                            "refresh_token": "custom-refresh-secret",
                            "expires_in": 3600,
                            "token_type": "Bearer",
                            "id_token": signed_fixture_id_token_with(
                                port,
                                client_id,
                                &nonce,
                                iat_offset_secs,
                            ),
                        });
                        if let Some(scope_value) = scope {
                            token_payload["scope"] = serde_json::json!(scope_value);
                        }
                        ("200 OK", token_payload.to_string())
                    }
                    1 => (
                        "200 OK",
                        serde_json::json!({"keys": [{
                            "kid": "fixture", "kty": "RSA", "alg": "RS256",
                            "n": FIXTURE_MODULUS, "e": "AQAB",
                        }]})
                        .to_string(),
                    ),
                    _ => (models_status, models_body.to_owned()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (port, send_nonce, handle)
    }

    #[test]
    fn list_plan_models_failures_still_persist_tokens_with_baseline_models() {
        let _flow_guard = lock_session_flow_test();
        let cases = [
            ("404 Not Found", r#"{"error":"not_found"}"#),
            ("500 Internal Server Error", r#"{"error":"internal"}"#),
            ("200 OK", "not-valid-json{{{"),
            ("200 OK", r#"{"models":[]}"#),
        ];

        for (models_status, models_body) in cases {
            let (port, send_nonce, server) = spawn_exchange_server(
                "oaiapp_fixture",
                Some(DEFAULT_SCOPES),
                0,
                models_status,
                models_body,
            );
            let keyring = MemoryKeyring::default();
            let config = ChatGptOAuthConfig {
                issuer: format!("http://127.0.0.1:{port}"),
                client_id: DYNAMIC_CLIENT_ID.to_string(),
                scopes: DEFAULT_SCOPES.to_string(),
                callback_ports: Vec::new(),
            };

            let status = run_oauth_login_flow_with(
                &keyring,
                &config,
                |auth_url| {
                    let parsed = Url::parse(auth_url).unwrap();
                    let query: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
                    send_nonce.send(query["nonce"].clone()).unwrap();
                    let cb_url = format!(
                        "{}?code=nonfatal-models-code&client_id=oaiapp_fixture&state={}",
                        query["redirect_uri"], query["state"]
                    );
                    std::thread::spawn(move || {
                        let _ = reqwest::blocking::get(cb_url);
                    });
                    Ok(())
                },
                Duration::from_secs(5),
                Duration::from_secs(5),
                None,
            )
            .expect("sign-in must succeed even when /v1/models fails");

            assert!(status.connected);
            assert_eq!(status.models, baseline_plan_models());
            let stored = ChatGptOAuthStore::load_with(&keyring)
                .unwrap()
                .expect("tokens persisted in keyring");
            assert_eq!(stored.access_token, "custom-access-secret");
            assert_eq!(
                stored.refresh_token.as_deref(),
                Some("custom-refresh-secret")
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn list_plan_models_parses_standard_openai_data_list() {
        let _flow_guard = lock_session_flow_test();
        let (port, server) = spawn_scripted_token_server(vec![(
            "200 OK",
            r#"{"object":"list","data":[{"id":"gpt-5"},{"id":"o4-mini"},{"id":"gpt-5"},{"id":"bad model!"}]}"#,
        )]);
        let config = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port}"),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };

        let models = list_plan_models(&config, "fixture-token", Duration::from_secs(5))
            .expect("OpenAI { data: [...] } list parses");
        assert_eq!(
            models,
            vec![
                PlanModel {
                    slug: "gpt-5".to_owned(),
                    display_name: "gpt-5".to_owned(),
                },
                PlanModel {
                    slug: "o4-mini".to_owned(),
                    display_name: "o4-mini".to_owned(),
                },
            ]
        );
        server.join().unwrap();
    }

    #[test]
    fn verify_id_token_accepts_60s_iat_clock_skew_and_rejects_excessive_skew() {
        let _flow_guard = lock_session_flow_test();
        let jwks = serde_json::json!({"keys": [{
            "kid": "fixture", "kty": "RSA", "alg": "RS256",
            "n": FIXTURE_MODULUS, "e": "AQAB",
        }]})
        .to_string();
        let (port, server) = spawn_owned_scripted_token_server(vec![
            ("200 OK", jwks.clone()),
            ("200 OK", jwks),
            (
                "500 Internal Server Error",
                r#"{"error":"jwks_down"}"#.to_owned(),
            ),
        ]);
        let config = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port}"),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };

        let skewed_60s = signed_fixture_id_token_with(port, "oaiapp_fixture", "nonce-1", 60);
        let subject = verify_id_token(
            &config,
            &skewed_60s,
            "oaiapp_fixture",
            "nonce-1",
            Duration::from_secs(5),
        )
        .expect("60s iat clock skew must be accepted within 120s leeway");
        assert_eq!(subject, "example-subject");

        let skewed_180s = signed_fixture_id_token_with(port, "oaiapp_fixture", "nonce-2", 180);
        let err = verify_id_token(
            &config,
            &skewed_180s,
            "oaiapp_fixture",
            "nonce-2",
            Duration::from_secs(5),
        )
        .expect_err("180s iat clock skew exceeds 120s leeway");
        assert_eq!(err, ChatGptOAuthError::IdTokenVerificationFailed);

        let jwks_err = verify_id_token(
            &config,
            &skewed_60s,
            "oaiapp_fixture",
            "nonce-1",
            Duration::from_secs(5),
        )
        .expect_err("JWKS 500 must map to JwksFetchFailed");
        assert_eq!(jwks_err, ChatGptOAuthError::JwksFetchFailed);

        server.join().unwrap();
    }

    #[test]
    fn rfc6749_omitted_scope_is_accepted_while_explicit_missing_plan_scope_is_rejected() {
        let _flow_guard = lock_session_flow_test();
        assert!(has_plan_scope(None));
        assert!(has_plan_scope(Some(DEFAULT_SCOPES)));
        assert!(!has_plan_scope(Some("openid profile email")));
        assert!(!has_plan_scope(Some("")));

        let (port, send_nonce, server) = spawn_exchange_server(
            "oaiapp_fixture",
            None,
            0,
            "200 OK",
            r#"{"data":[{"id":"gpt-5"}]}"#,
        );
        let keyring = MemoryKeyring::default();
        let config = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port}"),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };

        let status = run_oauth_login_flow_with(
            &keyring,
            &config,
            |auth_url| {
                let parsed = Url::parse(auth_url).unwrap();
                let query: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
                send_nonce.send(query["nonce"].clone()).unwrap();
                let cb_url = format!(
                    "{}?code=no-scope-code&client_id=oaiapp_fixture&state={}",
                    query["redirect_uri"], query["state"]
                );
                std::thread::spawn(move || {
                    let _ = reqwest::blocking::get(cb_url);
                });
                Ok(())
            },
            Duration::from_secs(5),
            Duration::from_secs(5),
            None,
        )
        .expect("token response omitting scope per RFC 6749 §5.1 must succeed");

        assert!(status.connected);
        assert_eq!(status.models.len(), 1);
        assert_eq!(status.models[0].slug, "gpt-5");
        server.join().unwrap();

        let (port_bad, send_nonce_bad, server_bad) = spawn_exchange_server(
            "oaiapp_fixture",
            Some("openid profile email"),
            0,
            "200 OK",
            r#"{"data":[{"id":"gpt-5"}]}"#,
        );
        let config_bad = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port_bad}"),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };
        let nonce_bad = "nonce-missing-plan-scope";
        send_nonce_bad.send(nonce_bad.to_owned()).unwrap();
        let err = exchange_code(
            &config_bad,
            AuthorizationCallback {
                code: "code-bad-scope".to_owned(),
                client_id: Some("oaiapp_fixture".to_owned()),
            },
            "http://127.0.0.1:1455/auth/callback",
            "verifier",
            nonce_bad,
            None,
            Duration::from_secs(5),
        )
        .expect_err("explicit scope lacking PLAN_SCOPE must be rejected");
        assert_eq!(err.error, ChatGptOAuthError::AuthorizationDenied);
        // Drain the unused 3rd accept by connecting once so the helper thread exits cleanly.
        let _ = TcpStream::connect(("127.0.0.1", port_bad));
        server_bad.join().unwrap();
    }

    #[test]
    fn issued_client_accepts_app_prefix_config_fallback_and_classifies_invalid_client_id() {
        let _flow_guard = lock_session_flow_test();
        assert!(is_issued_client("oaiapp_12345_abc-XYZ"));
        assert!(is_issued_client("app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(!is_issued_client("app_"));
        assert!(!is_issued_client("oaiapp_"));
        assert!(!is_issued_client("dynamic_agent_client"));
        assert!(!is_issued_client("app_invalid space"));
        assert!(!is_issued_client(&format!("app_{}", "a".repeat(253))));

        // 1. Callback with app_* client_id succeeds.
        let (port, send_nonce, server) = spawn_exchange_server(
            "app_TestClient_01",
            Some(DEFAULT_SCOPES),
            0,
            "200 OK",
            r#"{"data":[{"id":"gpt-5"}]}"#,
        );
        let config = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port}"),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };
        send_nonce.send("nonce-app-cb".to_owned()).unwrap();
        let record = exchange_code(
            &config,
            AuthorizationCallback {
                code: "code-app-prefix".to_owned(),
                client_id: Some("app_TestClient_01".to_owned()),
            },
            "http://127.0.0.1:1455/auth/callback",
            "verifier",
            "nonce-app-cb",
            None,
            Duration::from_secs(5),
        )
        .map_err(|err| err.error)
        .expect("app_* callback client_id must succeed");
        assert_eq!(
            record.identity.as_ref().map(|id| id.client_id.as_str()),
            Some("app_TestClient_01")
        );
        server.join().unwrap();

        // 2. Callback omitting client_id falls back to config.client_id when != DYNAMIC_CLIENT_ID.
        let (port2, send_nonce2, server2) = spawn_exchange_server(
            "app_ConfiguredFallback_02",
            Some(DEFAULT_SCOPES),
            0,
            "200 OK",
            r#"{"data":[{"id":"o3"}]}"#,
        );
        let config_fallback = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port2}"),
            client_id: "app_ConfiguredFallback_02".to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };
        send_nonce2.send("nonce-app-fallback".to_owned()).unwrap();
        let record2 = exchange_code(
            &config_fallback,
            AuthorizationCallback {
                code: "code-fallback".to_owned(),
                client_id: None,
            },
            "http://127.0.0.1:1455/auth/callback",
            "verifier",
            "nonce-app-fallback",
            None,
            Duration::from_secs(5),
        )
        .map_err(|err| err.error)
        .expect("omitted callback client_id falls back to configured app_* client_id");
        assert_eq!(
            record2.identity.as_ref().map(|id| id.client_id.as_str()),
            Some("app_ConfiguredFallback_02")
        );
        server2.join().unwrap();

        // 3. Callback omitting client_id with DYNAMIC_CLIENT_ID fails with CallbackClientIdInvalid.
        let missing_err = exchange_code(
            &config,
            AuthorizationCallback {
                code: "code-missing-cid".to_owned(),
                client_id: None,
            },
            "http://127.0.0.1:1455/auth/callback",
            "verifier",
            "nonce-missing",
            None,
            Duration::from_secs(5),
        )
        .expect_err("missing client_id with dynamic_agent_client must fail");
        assert_eq!(
            missing_err.error,
            ChatGptOAuthError::CallbackClientIdInvalid
        );
    }

    #[test]
    fn token_endpoint_http_rejection_classifies_status_code() {
        let _flow_guard = lock_session_flow_test();
        let (port, server) = spawn_scripted_token_server(vec![(
            "400 Bad Request",
            r#"{"error":"invalid_grant","error_description":"code expired"}"#,
        )]);
        let config = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port}"),
            client_id: DYNAMIC_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(),
        };
        let reject_err = exchange_code(
            &config,
            AuthorizationCallback {
                code: "code-expired".to_owned(),
                client_id: Some("app_TestClient_01".to_owned()),
            },
            "http://127.0.0.1:1455/auth/callback",
            "verifier",
            "nonce-reject",
            None,
            Duration::from_secs(5),
        )
        .expect_err("400 token response must fail with TokenEndpointRejected");
        assert_eq!(
            reject_err.error,
            ChatGptOAuthError::TokenEndpointRejected { status: 400 }
        );
        server.join().unwrap();
    }
}
