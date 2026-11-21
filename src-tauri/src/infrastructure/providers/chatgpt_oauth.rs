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
//! - Token exchange and refresh disable HTTP redirects so authorization codes
//!   and refresh tokens are never forwarded to unexpected hosts.

#![allow(clippy::doc_markdown)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
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
pub(crate) const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Default OAuth scopes requested during opt-in ChatGPT sign-in.
pub(crate) const DEFAULT_SCOPES: &str = "openid profile email offline_access";

/// Preferred loopback callback ports tried before falling back to an ephemeral
/// loopback port (`127.0.0.1:0`).
pub(crate) const DEFAULT_CALLBACK_PORTS: [u16; 2] = [1455, 1457];

/// Default wall-clock deadline waiting for the browser OAuth callback.
pub(crate) const DEFAULT_OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_mins(2);

/// Default HTTP timeout for token exchange and refresh requests.
pub(crate) const DEFAULT_OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// Refresh access tokens when fewer than this many seconds remain before expiry.
const REFRESH_SKEW_SECS: i64 = 60;

/// Maximum bytes read from a loopback HTTP callback request.
const MAX_CALLBACK_REQUEST_BYTES: usize = 8192;

/// Maximum bytes read from a token endpoint response.
const MAX_TOKEN_RESPONSE_BYTES: usize = 128 * 1024;

/// Maximum UTF-8 characters stored in a single OS keyring entry so every chunk
/// stays safely below Windows Credential Manager's 2560-byte (`1280` UTF-16
/// char) `CRED_MAX_CREDENTIAL_BLOB_SIZE` limit (`keyring::Error::TooLong`).
const MAX_KEYRING_CHUNK_CHARS: usize = 1000;

/// Maximum number of keyring chunks allowed for one OAuth token bundle.
const MAX_KEYRING_CHUNKS: usize = 32;

/// Single-flight lock serializing OAuth token refresh operations so concurrent
/// callers near expiry never race on a single-use rotating `refresh_token`.
static REFRESH_SINGLE_FLIGHT: Mutex<()> = Mutex::new(());

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
    /// Refreshing the stored OAuth token failed or no refresh token is stored.
    TokenRefreshFailed,
    /// Token endpoint response exceeded the safety size limit.
    ResponseTooLarge,
    /// No ChatGPT OAuth session is currently stored in the keyring.
    NotConnected,
    /// The OAuth sign-in was cancelled before completion.
    Cancelled,
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
            Self::TokenRefreshFailed => {
                write!(
                    f,
                    "ChatGPT OAuth token refresh failed; please sign in again"
                )
            }
            Self::ResponseTooLarge => {
                write!(f, "ChatGPT OAuth token response exceeded the safety limit")
            }
            Self::NotConnected => write!(f, "ChatGPT OAuth is not connected"),
            Self::Cancelled => write!(f, "ChatGPT OAuth sign-in was cancelled"),
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
            client_id: DEFAULT_CLIENT_ID.to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: DEFAULT_CALLBACK_PORTS.to_vec(),
        }
    }
}

impl ChatGptOAuthConfig {
    /// Validate and build the authorization endpoint URL.
    fn authorize_endpoint(&self) -> Result<Url, ChatGptOAuthError> {
        let base = format!("{}/oauth/authorize", self.issuer.trim_end_matches('/'));
        validate_oauth_endpoint_url(&base)
    }

    /// Validate and build the token endpoint URL.
    fn token_endpoint(&self) -> Result<Url, ChatGptOAuthError> {
        let base = format!("{}/oauth/token", self.issuer.trim_end_matches('/'));
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ChatGptOAuthStatus {
    pub connected: bool,
    pub has_refresh_token: bool,
    pub expires_at: Option<i64>,
    pub connected_at: Option<i64>,
    pub auth_mode: &'static str,
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
        }
    }
}

/// Secret OAuth token bundle persisted EXCLUSIVELY inside the OS keyring.
/// Never serialized to `SQLite`, IPC payloads, or logs.
/// Unused `id_token` payloads are deliberately dropped before persistence so
/// keyring entries stay compact across platforms.
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
    pub(crate) fn status(&self) -> ChatGptOAuthStatus {
        let connected = !self.access_token.trim().is_empty();
        ChatGptOAuthStatus {
            connected,
            has_refresh_token: self
                .refresh_token
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()),
            expires_at: self.expires_at,
            connected_at: if connected && self.connected_at > 0 {
                Some(self.connected_at)
            } else {
                None
            },
            auth_mode: if connected { "oauth" } else { "none" },
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
}

fn chunk_entry_key(slot: u8, index: usize) -> String {
    format!("{CHATGPT_OAUTH_KEYRING_KEY}.s{slot}.{index}")
}

fn split_into_keyring_chunks(payload: &str) -> Result<Vec<String>, ChatGptOAuthError> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut char_count = 0usize;
    for ch in payload.chars() {
        current.push(ch);
        char_count += 1;
        if char_count == MAX_KEYRING_CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
            char_count = 0;
        }
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
    pub(crate) fn save_with(
        backend: &dyn CredentialBackend,
        tokens: &StoredChatGptOAuthTokens,
    ) -> Result<(), ChatGptOAuthError> {
        let payload =
            serde_json::to_string(tokens).map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
        let chunks = split_into_keyring_chunks(&payload)?;

        let prev_manifest =
            match backend.get_password(CHATGPT_OAUTH_KEYRING_SERVICE, CHATGPT_OAUTH_KEYRING_KEY) {
                Ok(raw) => serde_json::from_str::<KeyringChunkManifest>(&raw).ok(),
                Err(_) => None,
            };
        let next_slot = match prev_manifest {
            Some(m) if m.slot == 0 => 1,
            _ => 0,
        };

        for (idx, chunk) in chunks.iter().enumerate() {
            let key = chunk_entry_key(next_slot, idx);
            if backend
                .set_password(CHATGPT_OAUTH_KEYRING_SERVICE, &key, chunk)
                .is_err()
            {
                // Roll back partially written chunks in `next_slot` without
                // touching the active manifest or the previous slot's chunks.
                for cleanup_idx in 0..idx {
                    let _ = backend.delete_credential(
                        CHATGPT_OAUTH_KEYRING_SERVICE,
                        &chunk_entry_key(next_slot, cleanup_idx),
                    );
                }
                return Err(ChatGptOAuthError::KeyringUnavailable);
            }
        }

        let manifest = KeyringChunkManifest {
            slot: next_slot,
            chunks: chunks.len(),
        };
        let manifest_json =
            serde_json::to_string(&manifest).map_err(|_| ChatGptOAuthError::KeyringUnavailable)?;
        if backend
            .set_password(
                CHATGPT_OAUTH_KEYRING_SERVICE,
                CHATGPT_OAUTH_KEYRING_KEY,
                &manifest_json,
            )
            .is_err()
        {
            for cleanup_idx in 0..chunks.len() {
                let _ = backend.delete_credential(
                    CHATGPT_OAUTH_KEYRING_SERVICE,
                    &chunk_entry_key(next_slot, cleanup_idx),
                );
            }
            return Err(ChatGptOAuthError::KeyringUnavailable);
        }

        if let Some(prev) = prev_manifest {
            if prev.slot != next_slot {
                for old_idx in 0..prev.chunks.min(MAX_KEYRING_CHUNKS) {
                    let _ = backend.delete_credential(
                        CHATGPT_OAUTH_KEYRING_SERVICE,
                        &chunk_entry_key(prev.slot, old_idx),
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn load_with(
        backend: &dyn CredentialBackend,
    ) -> Result<Option<StoredChatGptOAuthTokens>, ChatGptOAuthError> {
        let raw =
            match backend.get_password(CHATGPT_OAUTH_KEYRING_SERVICE, CHATGPT_OAUTH_KEYRING_KEY) {
                Ok(raw) => raw,
                Err(keyring::Error::NoEntry) => return Ok(None),
                Err(_) => return Err(ChatGptOAuthError::KeyringUnavailable),
            };

        if let Ok(manifest) = serde_json::from_str::<KeyringChunkManifest>(&raw) {
            if manifest.chunks == 0 || manifest.chunks > MAX_KEYRING_CHUNKS {
                return Err(ChatGptOAuthError::KeyringUnavailable);
            }
            let mut assembled = String::with_capacity(manifest.chunks * MAX_KEYRING_CHUNK_CHARS);
            for idx in 0..manifest.chunks {
                let chunk = backend
                    .get_password(
                        CHATGPT_OAUTH_KEYRING_SERVICE,
                        &chunk_entry_key(manifest.slot, idx),
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
        Self::clear_with(&KeyringBackend)
    }

    pub(crate) fn clear_with(backend: &dyn CredentialBackend) -> Result<(), ChatGptOAuthError> {
        let prev_manifest =
            match backend.get_password(CHATGPT_OAUTH_KEYRING_SERVICE, CHATGPT_OAUTH_KEYRING_KEY) {
                Ok(raw) => serde_json::from_str::<KeyringChunkManifest>(&raw).ok(),
                Err(keyring::Error::NoEntry) => None,
                Err(_) => return Err(ChatGptOAuthError::KeyringUnavailable),
            };

        if let Some(manifest) = prev_manifest {
            for idx in 0..manifest.chunks.min(MAX_KEYRING_CHUNKS) {
                match backend.delete_credential(
                    CHATGPT_OAUTH_KEYRING_SERVICE,
                    &chunk_entry_key(manifest.slot, idx),
                ) {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(_) => return Err(ChatGptOAuthError::KeyringUnavailable),
                }
            }
        }

        match backend.delete_credential(CHATGPT_OAUTH_KEYRING_SERVICE, CHATGPT_OAUTH_KEYRING_KEY) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(ChatGptOAuthError::KeyringUnavailable),
        }
    }

    /// Report secret-free connection status from the OS keyring.
    pub(crate) fn status() -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
        Self::status_with(&KeyringBackend)
    }

    pub(crate) fn status_with(
        backend: &dyn CredentialBackend,
    ) -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
        Ok(Self::load_with(backend)?.map_or_else(ChatGptOAuthStatus::disconnected, |t| t.status()))
    }

    /// Report whether a valid opt-in ChatGPT OAuth token entry exists.
    pub(crate) fn is_connected() -> Result<bool, ChatGptOAuthError> {
        Self::is_connected_with(&KeyringBackend)
    }

    pub(crate) fn is_connected_with(
        backend: &dyn CredentialBackend,
    ) -> Result<bool, ChatGptOAuthError> {
        Ok(Self::status_with(backend)?.connected)
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
) -> Result<String, ChatGptOAuthError> {
    let mut url = config.authorize_endpoint()?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("response_type", "code");
        pairs.append_pair("client_id", config.client_id.trim());
        pairs.append_pair("redirect_uri", redirect_uri);
        pairs.append_pair("code_challenge", &pkce.challenge);
        pairs.append_pair("code_challenge_method", "S256");
        pairs.append_pair("state", state);
        if !config.scopes.trim().is_empty() {
            pairs.append_pair("scope", config.scopes.trim());
        }
        pairs.append_pair("id_token_add_organizations", "true");
        pairs.append_pair("codex_cli_simplified_flow", "true");
    }
    Ok(url.into())
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
    pub(crate) fn wait_for_code(
        &self,
        expected_state: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, ChatGptOAuthError> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Err(ChatGptOAuthError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(ChatGptOAuthError::CallbackTimeout);
            }
            match self.listener.accept() {
                Ok((mut stream, _peer)) => {
                    if let Some(outcome) =
                        handle_callback_connection(&mut stream, expected_state, deadline, cancel)
                    {
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
    cancel: Option<&AtomicBool>,
) -> Option<Result<String, ChatGptOAuthError>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));

    let mut buf = [0u8; MAX_CALLBACK_REQUEST_BYTES];
    let mut filled = 0usize;
    while filled < MAX_CALLBACK_REQUEST_BYTES {
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Some(Err(ChatGptOAuthError::Cancelled));
        }
        let now = Instant::now();
        if now >= deadline {
            return Some(Err(ChatGptOAuthError::CallbackTimeout));
        }
        let read_timeout = deadline
            .saturating_duration_since(now)
            .min(Duration::from_secs(1))
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
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
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

    if parsed.path() != "/auth/callback" && parsed.path() != "/callback" {
        write_static_http_response(stream, "404 Not Found", "Not found.");
        return None;
    }

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut error: Option<String> = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
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
        "Sign-in complete. You may close this tab and return to Nexora.",
    );
    Some(Ok(auth_code))
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

fn post_token_form(
    token_url: Url,
    form: &[(&str, &str)],
    timeout: Duration,
    failure_kind: ChatGptOAuthError,
) -> Result<StoredChatGptOAuthTokens, ChatGptOAuthError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| failure_kind)?;

    let encoded_form: String = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form.iter().copied())
        .finish();

    let response = client
        .post(token_url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encoded_form)
        .send()
        .map_err(|_| failure_kind)?;

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

    let raw: RawTokenResponse = serde_json::from_slice(&body).map_err(|_| failure_kind)?;
    if raw.access_token.trim().is_empty() || raw.access_token.chars().any(char::is_control) {
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
    })
}

/// Exchange an OAuth authorization `code` + PKCE `code_verifier` for tokens and
/// persist them exclusively in the OS keyring.
///
/// # Errors
/// Returns a secret-free [`ChatGptOAuthError`].
pub(crate) fn exchange_code_and_store_with(
    backend: &dyn CredentialBackend,
    config: &ChatGptOAuthConfig,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    timeout: Duration,
) -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
    let token_url = config.token_endpoint()?;
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", config.client_id.trim()),
        ("code_verifier", code_verifier),
    ];
    let record = post_token_form(
        token_url,
        &form,
        timeout,
        ChatGptOAuthError::TokenExchangeFailed,
    )?;
    let status = record.status();
    ChatGptOAuthStore::save_with(backend, &record)?;
    Ok(status)
}

fn refresh_tokens_locked(
    backend: &dyn CredentialBackend,
    config: &ChatGptOAuthConfig,
    timeout: Duration,
) -> Result<StoredChatGptOAuthTokens, ChatGptOAuthError> {
    let existing = ChatGptOAuthStore::load_with(backend)?.ok_or(ChatGptOAuthError::NotConnected)?;
    let refresh = existing
        .refresh_token
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or(ChatGptOAuthError::TokenRefreshFailed)?;

    let token_url = config.token_endpoint()?;
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", config.client_id.trim()),
    ];
    let mut updated = post_token_form(
        token_url,
        &form,
        timeout,
        ChatGptOAuthError::TokenRefreshFailed,
    )?;
    // Preserve previous refresh token and initial connected_at timestamp if not rotated.
    if updated.refresh_token.is_none() {
        updated.refresh_token = existing.refresh_token;
    }
    if existing.connected_at > 0 {
        updated.connected_at = existing.connected_at;
    }
    ChatGptOAuthStore::save_with(backend, &updated)?;
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
pub(crate) fn refresh_tokens_with(
    backend: &dyn CredentialBackend,
    config: &ChatGptOAuthConfig,
    timeout: Duration,
) -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
    let _guard = REFRESH_SINGLE_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let updated = refresh_tokens_locked(backend, config, timeout)?;
    Ok(updated.status())
}

/// Refresh stored ChatGPT OAuth tokens in production using the default OS
/// keyring and issuer config.
///
/// # Errors
/// Returns a secret-free [`ChatGptOAuthError`].
pub(crate) fn refresh_tokens() -> Result<ChatGptOAuthStatus, ChatGptOAuthError> {
    refresh_tokens_with(
        &KeyringBackend,
        &ChatGptOAuthConfig::default(),
        DEFAULT_OAUTH_HTTP_TIMEOUT,
    )
}

/// Resolve an active ChatGPT OAuth Bearer access token from the OS keyring,
/// transparently refreshing expired or near-expiry tokens under a single-flight
/// lock and failing closed if no usable `refresh_token` is available.
///
/// Returns `Ok(None)` when opt-in ChatGPT OAuth is not connected.
///
/// # Errors
/// Returns a secret-free [`ChatGptOAuthError`].
pub(crate) fn resolve_access_token_with(
    backend: &dyn CredentialBackend,
    config: &ChatGptOAuthConfig,
    timeout: Duration,
) -> Result<Option<String>, ChatGptOAuthError> {
    let Some(record) = ChatGptOAuthStore::load_with(backend)? else {
        return Ok(None);
    };
    if record.access_token.trim().is_empty() {
        return Ok(None);
    }
    if !record.needs_refresh(now_unix_secs()) {
        return Ok(Some(record.access_token));
    }

    let _guard = REFRESH_SINGLE_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(current) = ChatGptOAuthStore::load_with(backend)? else {
        return Ok(None);
    };
    if current.access_token.trim().is_empty() {
        return Ok(None);
    }
    if !current.needs_refresh(now_unix_secs()) {
        return Ok(Some(current.access_token));
    }

    let refreshed = refresh_tokens_locked(backend, config, timeout)?;
    Ok(Some(refreshed.access_token))
}

/// Run the full interactive opt-in ChatGPT OAuth 2.0 + PKCE loopback login flow
/// against `backend` and `config`.
///
/// # Errors
/// Returns a secret-free [`ChatGptOAuthError`].
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
    // Validate endpoints up front before binding sockets or opening the browser.
    let _ = config.authorize_endpoint()?;
    let _ = config.token_endpoint()?;

    let server = LoopbackCallbackServer::bind(&config.callback_ports)?;
    let redirect_uri = server.redirect_uri();
    let pkce = generate_pkce();
    let state = generate_csrf_state();
    let auth_url = build_authorization_url(config, &redirect_uri, &pkce, &state)?;

    open_browser(&auth_url)?;

    let code = server.wait_for_code(&state, callback_timeout, cancel)?;
    exchange_code_and_store_with(
        backend,
        config,
        &code,
        &redirect_uri,
        &pkce.verifier,
        http_timeout,
    )
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
    run_oauth_login_flow_with(
        &KeyringBackend,
        &ChatGptOAuthConfig::default(),
        open_browser,
        callback_timeout,
        DEFAULT_OAUTH_HTTP_TIMEOUT,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryKeyring {
        entries: Mutex<HashMap<(String, String), String>>,
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
        assert_eq!(code, "auth-code-777");
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
                scope: None,
                connected_at: 1_700_000_000,
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
                scope: None,
                connected_at: 1_700_000_000,
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
                scope: None,
                connected_at: 1_700_000_000,
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

    #[test]
    fn full_oauth_flow_exchanges_refreshes_and_keeps_tokens_in_keyring_only() {
        let token_listener = TcpListener::bind("127.0.0.1:0").expect("bind mock token server");
        let port = token_listener.local_addr().expect("addr").port();
        let server_thread = std::thread::spawn(move || {
            for idx in 0..2 {
                let (mut stream, _) = token_listener.accept().expect("accept");
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if idx == 0 {
                    assert!(req.contains("grant_type=authorization_code"));
                    assert!(req.contains("code=live-code-42"));
                    assert!(req.contains("code_verifier="));
                    // Short expires_in=10 so next resolve triggers automatic refresh (skew=60s).
                    r#"{"access_token":"chatgpt-initial-access-secret","refresh_token":"chatgpt-refresh-secret-1","expires_in":10,"token_type":"Bearer"}"#
                } else {
                    assert!(req.contains("grant_type=refresh_token"));
                    assert!(req.contains("refresh_token=chatgpt-refresh-secret-1"));
                    r#"{"access_token":"chatgpt-refreshed-access-secret","expires_in":3600,"token_type":"Bearer"}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let keyring = MemoryKeyring::default();
        let config = ChatGptOAuthConfig {
            issuer: format!("http://127.0.0.1:{port}"),
            client_id: "nexora-test-client".to_string(),
            scopes: DEFAULT_SCOPES.to_string(),
            callback_ports: Vec::new(), // Ephemeral port for deterministic test.
        };

        let status = run_oauth_login_flow_with(
            &keyring,
            &config,
            |auth_url| {
                let parsed = Url::parse(auth_url).expect("valid auth url");
                let mut redirect = None;
                let mut state = None;
                let mut method = None;
                for (k, v) in parsed.query_pairs() {
                    match k.as_ref() {
                        "redirect_uri" => redirect = Some(v.into_owned()),
                        "state" => state = Some(v.into_owned()),
                        "code_challenge_method" => method = Some(v.into_owned()),
                        _ => {}
                    }
                }
                assert_eq!(method.as_deref(), Some("S256"));
                let cb_url = format!(
                    "{}?code=live-code-42&state={}",
                    redirect.expect("redirect_uri"),
                    state.expect("state")
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
            ChatGptOAuthError::TokenRefreshFailed,
            ChatGptOAuthError::ResponseTooLarge,
            ChatGptOAuthError::NotConnected,
            ChatGptOAuthError::Cancelled,
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
            scope: None,
            connected_at: 1_899_996_400,
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
}
