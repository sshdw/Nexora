//! OAuth 2.1 + PKCE (`S256`) flow and OS keyring-only credential storage for
//! user-added MCP servers.
//!
//! Security invariants:
//! - Access tokens, refresh tokens, and bearer tokens live ONLY in the OS
//!   secure keyring (`keyring` crate via [`CredentialBackend`]) — NEVER in
//!   `SQLite`, IPC payloads, error messages, or logs.
//! - PKCE uses `S256` (`SHA-256` + `base64url` no-pad) with 256-bit CSPRNG
//!   verifiers (`uuid::Uuid::new_v4`).
//! - CSRF `state` is 256-bit CSPRNG and validated in constant time.
//! - Callback listener binds strictly to `127.0.0.1:0` (ephemeral loopback
//!   port) with a hard timeout and static XSS-free HTML responses.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use crate::application::import::McpOAuthConfig;
use crate::infrastructure::providers::credentials::{CredentialBackend, KeyringBackend};

use super::url_safety::{build_hardened_http_client, validate_mcp_url};
use super::McpError;

/// Dedicated OS keyring service namespace for MCP credentials (kept separate
/// from AI provider credentials under `"nexora"` so server names can never
/// collide with provider names like `"openai"` or `"anthropic"`).
pub(crate) const MCP_KEYRING_SERVICE: &str = "nexora-mcp";

/// Default timeout waiting for the user to complete OAuth in their browser.
pub(crate) const DEFAULT_OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_mins(2);

/// Default HTTP timeout for token exchange / refresh requests.
pub(crate) const DEFAULT_OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// Refresh OAuth access tokens this many seconds before expiry.
const REFRESH_SKEW_SECS: i64 = 30;

/// Maximum bytes read from a loopback callback HTTP request.
const MAX_CALLBACK_REQUEST_BYTES: usize = 8192;

/// Maximum bytes read from a token endpoint response.
const MAX_TOKEN_RESPONSE_BYTES: usize = 128 * 1024;

/// Stored credential kind inside the OS keyring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum McpAuthKind {
    None,
    Bearer,
    Oauth,
}

impl McpAuthKind {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bearer => "bearer",
            Self::Oauth => "oauth",
        }
    }
}

/// Secret-free auth status returned across IPC and UI layers.
/// Never carries tokens or secret material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct McpAuthStatus {
    pub has_auth: bool,
    pub auth_kind: &'static str,
    pub expires_at: Option<i64>,
}

impl McpAuthStatus {
    #[must_use]
    pub(crate) const fn none() -> Self {
        Self {
            has_auth: false,
            auth_kind: McpAuthKind::None.as_str(),
            expires_at: None,
        }
    }
}

/// Secret record persisted EXCLUSIVELY inside the OS keyring.
/// Never serialized to `SQLite`, IPC, or logs.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredMcpAuth {
    pub kind: McpAuthKind,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl std::fmt::Debug for StoredMcpAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredMcpAuth")
            .field("kind", &self.kind)
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl StoredMcpAuth {
    #[must_use]
    pub(crate) fn status(&self) -> McpAuthStatus {
        McpAuthStatus {
            has_auth: !self.access_token.trim().is_empty(),
            auth_kind: self.kind.as_str(),
            expires_at: self.expires_at,
        }
    }

    #[must_use]
    pub(crate) fn needs_refresh(&self, now_secs: i64) -> bool {
        self.kind == McpAuthKind::Oauth
            && self
                .expires_at
                .is_some_and(|exp| now_secs.saturating_add(REFRESH_SKEW_SECS) >= exp)
    }
}

/// Keyring entry key for an MCP server's auth record.
fn keyring_entry_key(server_name: &str) -> String {
    format!("mcp-auth:{server_name}")
}

/// OS keyring store for MCP credentials (Bearer tokens & OAuth 2.1 tokens).
pub(crate) struct McpCredentialStore;

impl McpCredentialStore {
    pub(crate) fn store_bearer(server_name: &str, token: &str) -> Result<(), McpError> {
        Self::store_bearer_with(&KeyringBackend, server_name, token)
    }

    pub(crate) fn store_bearer_with(
        backend: &dyn CredentialBackend,
        server_name: &str,
        token: &str,
    ) -> Result<(), McpError> {
        let trimmed = token.trim();
        if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
            return Err(McpError::InvalidConfig(
                "bearer token must be non-empty without control characters",
            ));
        }
        let record = StoredMcpAuth {
            kind: McpAuthKind::Bearer,
            access_token: trimmed.to_string(),
            refresh_token: None,
            expires_at: None,
            token_type: Some("Bearer".to_string()),
            scope: None,
        };
        Self::save_record_with(backend, server_name, &record)
    }

    pub(crate) fn save_record(server_name: &str, record: &StoredMcpAuth) -> Result<(), McpError> {
        Self::save_record_with(&KeyringBackend, server_name, record)
    }

    pub(crate) fn save_record_with(
        backend: &dyn CredentialBackend,
        server_name: &str,
        record: &StoredMcpAuth,
    ) -> Result<(), McpError> {
        let key = keyring_entry_key(server_name);
        let payload = serde_json::to_string(record).map_err(|_| McpError::KeyringUnavailable)?;
        backend
            .set_password(MCP_KEYRING_SERVICE, &key, &payload)
            .map_err(|_| McpError::KeyringUnavailable)
    }

    pub(crate) fn load_record(server_name: &str) -> Result<Option<StoredMcpAuth>, McpError> {
        Self::load_record_with(&KeyringBackend, server_name)
    }

    pub(crate) fn load_record_with(
        backend: &dyn CredentialBackend,
        server_name: &str,
    ) -> Result<Option<StoredMcpAuth>, McpError> {
        let key = keyring_entry_key(server_name);
        match backend.get_password(MCP_KEYRING_SERVICE, &key) {
            Ok(raw) => {
                let parsed: StoredMcpAuth =
                    serde_json::from_str(&raw).map_err(|_| McpError::KeyringUnavailable)?;
                Ok(Some(parsed))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(McpError::KeyringUnavailable),
        }
    }

    pub(crate) fn remove(server_name: &str) -> Result<(), McpError> {
        Self::remove_with(&KeyringBackend, server_name)
    }

    pub(crate) fn remove_with(
        backend: &dyn CredentialBackend,
        server_name: &str,
    ) -> Result<(), McpError> {
        let key = keyring_entry_key(server_name);
        match backend.delete_credential(MCP_KEYRING_SERVICE, &key) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(McpError::KeyringUnavailable),
        }
    }

    pub(crate) fn status(server_name: &str) -> McpAuthStatus {
        Self::status_with(&KeyringBackend, server_name)
    }

    pub(crate) fn status_with(backend: &dyn CredentialBackend, server_name: &str) -> McpAuthStatus {
        match Self::load_record_with(backend, server_name) {
            Ok(Some(record)) => record.status(),
            Ok(None) | Err(_) => McpAuthStatus::none(),
        }
    }
}

/// PKCE `S256` challenge pair (RFC 7636).
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

/// Generate a 256-bit cryptographically random URL-safe token using OS CSPRNG
/// (`Uuid::new_v4` x 2 = 32 random bytes -> 43 base64url characters).
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

/// Generate a fresh PKCE `S256` verifier + challenge pair.
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

/// Constant-time equality for CSRF `state` verification.
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

/// Build the OAuth 2.1 authorization URL with PKCE `S256` and CSRF `state`.
///
/// # Errors
/// Returns [`McpError::InvalidUrl`] if `oauth.authorization_url` fails strict
/// URL/SSRF validation.
pub(crate) fn build_authorization_url(
    oauth: &McpOAuthConfig,
    redirect_uri: &str,
    pkce: &PkcePair,
    state: &str,
) -> Result<String, McpError> {
    let mut url = validate_mcp_url(&oauth.authorization_url)
        .map_err(|reason| McpError::InvalidUrl(reason.message()))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("response_type", "code");
        pairs.append_pair("client_id", oauth.client_id.trim());
        pairs.append_pair("redirect_uri", redirect_uri);
        pairs.append_pair("code_challenge", &pkce.challenge);
        pairs.append_pair("code_challenge_method", "S256");
        pairs.append_pair("state", state);
        if !oauth.scopes.is_empty() {
            pairs.append_pair("scope", &oauth.scopes.join(" "));
        }
    }
    Ok(url.into())
}

/// Ephemeral loopback OAuth callback listener bound strictly to `127.0.0.1:0`.
pub(crate) struct LoopbackCallbackServer {
    listener: TcpListener,
    addr: SocketAddr,
}

impl LoopbackCallbackServer {
    /// Bind an ephemeral port on `127.0.0.1`.
    ///
    /// # Errors
    /// Returns [`McpError::OAuthFailed`] if binding the loopback socket fails.
    pub(crate) fn bind() -> Result<Self, McpError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|_| McpError::OAuthFailed("failed to bind loopback OAuth callback socket"))?;
        listener
            .set_nonblocking(true)
            .map_err(|_| McpError::OAuthFailed("failed to configure OAuth callback socket"))?;
        let addr = listener
            .local_addr()
            .map_err(|_| McpError::OAuthFailed("failed to inspect OAuth callback port"))?;
        Ok(Self { listener, addr })
    }

    #[must_use]
    pub(crate) fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.addr.port())
    }

    /// Wait for the OAuth authorization callback, validating the CSRF `state`
    /// parameter in constant time and returning the authorization `code`.
    ///
    /// # Errors
    /// Returns [`McpError::OAuthTimeout`], [`McpError::OAuthStateMismatch`],
    /// [`McpError::Cancelled`], or [`McpError::OAuthFailed`].
    pub(crate) fn wait_for_code(
        &self,
        expected_state: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, McpError> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Err(McpError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(McpError::OAuthTimeout);
            }
            match self.listener.accept() {
                Ok((mut stream, _peer)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

                    let mut buf = [0u8; MAX_CALLBACK_REQUEST_BYTES];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let Some(target) = parse_http_get_target(&request) else {
                        write_static_http_response(
                            &mut stream,
                            "400 Bad Request",
                            "Invalid OAuth callback request.",
                        );
                        continue;
                    };

                    if !target.starts_with("/callback") {
                        write_static_http_response(&mut stream, "404 Not Found", "Not found.");
                        continue;
                    }

                    let parsed = Url::parse(&format!("http://127.0.0.1{target}"))
                        .map_err(|_| McpError::OAuthFailed("malformed OAuth callback URL"))?;

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

                    if error.is_some() {
                        write_static_http_response(
                            &mut stream,
                            "400 Bad Request",
                            "Authorization was denied or cancelled. You may close this tab.",
                        );
                        return Err(McpError::OAuthFailed(
                            "authorization server returned an error",
                        ));
                    }

                    let state_valid = state.as_deref().is_some_and(|received| {
                        constant_time_eq(received.as_bytes(), expected_state.as_bytes())
                    });
                    if !state_valid {
                        write_static_http_response(
                            &mut stream,
                            "400 Bad Request",
                            "OAuth state verification failed. You may close this tab.",
                        );
                        return Err(McpError::OAuthStateMismatch);
                    }

                    let Some(auth_code) = code.filter(|c| !c.trim().is_empty()) else {
                        write_static_http_response(
                            &mut stream,
                            "400 Bad Request",
                            "Missing authorization code. You may close this tab.",
                        );
                        return Err(McpError::OAuthFailed(
                            "OAuth callback did not contain an authorization code",
                        ));
                    };

                    write_static_http_response(
                        &mut stream,
                        "200 OK",
                        "Authorization complete. You may close this tab and return to Nexora.",
                    );
                    return Ok(auth_code);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => {
                    return Err(McpError::OAuthFailed("OAuth callback listener failed"));
                }
            }
        }
    }
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

fn write_static_http_response(stream: &mut std::net::TcpStream, status: &str, body_text: &str) {
    let html = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Nexora OAuth</title></head>\
         <body><p>{body_text}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
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
    token_url: &str,
    form: &[(&str, &str)],
    timeout: Duration,
) -> Result<StoredMcpAuth, McpError> {
    let parsed_url =
        validate_mcp_url(token_url).map_err(|reason| McpError::InvalidUrl(reason.message()))?;
    let client = build_hardened_http_client(&parsed_url, timeout)
        .map_err(|reason| McpError::InvalidUrl(reason.message()))?;

    let encoded_form: String = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form.iter().copied())
        .finish();
    let response = client
        .post(parsed_url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encoded_form)
        .send()
        .map_err(|_| McpError::OAuthFailed("token endpoint request failed"))?;

    if !response.status().is_success() {
        return Err(McpError::OAuthFailed(
            "token endpoint rejected the OAuth request",
        ));
    }

    let mut body = Vec::new();
    response
        .take(MAX_TOKEN_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| McpError::OAuthFailed("failed to read token endpoint response"))?;
    if body.len() > MAX_TOKEN_RESPONSE_BYTES {
        return Err(McpError::ResponseTooLarge);
    }

    let raw: RawTokenResponse = serde_json::from_slice(&body)
        .map_err(|_| McpError::OAuthFailed("token endpoint returned an invalid payload"))?;
    if raw.access_token.trim().is_empty() || raw.access_token.chars().any(char::is_control) {
        return Err(McpError::OAuthFailed(
            "token endpoint returned an invalid access token",
        ));
    }
    let expires_at = raw
        .expires_in
        .filter(|&secs| secs > 0)
        .map(|secs| now_unix_secs().saturating_add(secs));

    Ok(StoredMcpAuth {
        kind: McpAuthKind::Oauth,
        access_token: raw.access_token,
        refresh_token: raw.refresh_token.filter(|s| !s.trim().is_empty()),
        expires_at,
        token_type: raw.token_type,
        scope: raw.scope,
    })
}

/// Exchange an OAuth 2.1 authorization code + PKCE `code_verifier` for tokens
/// and persist them exclusively in the OS keyring.
///
/// # Errors
/// Returns secret-free [`McpError`].
pub(crate) fn exchange_code_and_store_with(
    backend: &dyn CredentialBackend,
    server_name: &str,
    oauth: &McpOAuthConfig,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    timeout: Duration,
) -> Result<McpAuthStatus, McpError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", oauth.client_id.trim()),
        ("code_verifier", code_verifier),
    ];
    let record = post_token_form(&oauth.token_url, &form, timeout)?;
    let status = record.status();
    McpCredentialStore::save_record_with(backend, server_name, &record)?;
    Ok(status)
}

/// Refresh stored OAuth 2.1 tokens using the keyring `refresh_token` and
/// persist the rotated tokens exclusively in the OS keyring.
///
/// # Errors
/// Returns secret-free [`McpError`].
pub(crate) fn refresh_oauth_token_with(
    backend: &dyn CredentialBackend,
    server_name: &str,
    oauth: &McpOAuthConfig,
    timeout: Duration,
) -> Result<McpAuthStatus, McpError> {
    let existing = McpCredentialStore::load_record_with(backend, server_name)?
        .ok_or(McpError::Unauthorized)?;
    let refresh = existing
        .refresh_token
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or(McpError::Unauthorized)?;

    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", oauth.client_id.trim()),
    ];
    let mut updated = post_token_form(&oauth.token_url, &form, timeout)?;
    // Preserve previous refresh token if the server did not rotate it.
    if updated.refresh_token.is_none() {
        updated.refresh_token = existing.refresh_token;
    }
    let status = updated.status();
    McpCredentialStore::save_record_with(backend, server_name, &updated)?;
    Ok(status)
}

/// Resolve the active Bearer access token from the OS keyring, transparently
/// refreshing expired OAuth 2.1 tokens when `oauth` config and a refresh token
/// are present.
pub(crate) fn resolve_bearer_token_with(
    backend: &dyn CredentialBackend,
    server_name: &str,
    oauth: Option<&McpOAuthConfig>,
    timeout: Duration,
) -> Result<Option<String>, McpError> {
    let Some(record) = McpCredentialStore::load_record_with(backend, server_name)? else {
        return Ok(None);
    };
    if record.needs_refresh(now_unix_secs()) {
        if let Some(cfg) = oauth {
            if record.refresh_token.is_some() {
                refresh_oauth_token_with(backend, server_name, cfg, timeout)?;
                let fresh = McpCredentialStore::load_record_with(backend, server_name)?
                    .ok_or(McpError::Unauthorized)?;
                return Ok(Some(fresh.access_token));
            }
        }
    }
    Ok(Some(record.access_token))
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
            self.entries.lock().unwrap().insert(
                (service.to_string(), key.to_string()),
                credential.to_string(),
            );
            Ok(())
        }

        fn get_password(&self, service: &str, key: &str) -> keyring::Result<String> {
            self.entries
                .lock()
                .unwrap()
                .get(&(service.to_string(), key.to_string()))
                .cloned()
                .ok_or(keyring::Error::NoEntry)
        }

        fn delete_credential(&self, service: &str, key: &str) -> keyring::Result<()> {
            self.entries
                .lock()
                .unwrap()
                .remove(&(service.to_string(), key.to_string()))
                .map(|_| ())
                .ok_or(keyring::Error::NoEntry)
        }
    }

    #[test]
    fn pkce_s256_matches_rfc7636_vector() {
        // RFC 7636 Appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(compute_s256_challenge(verifier), expected);

        let pair = generate_pkce();
        assert_eq!(pair.verifier.len(), 43);
        assert_eq!(compute_s256_challenge(&pair.verifier), pair.challenge);
    }

    #[test]
    fn loopback_callback_accepts_valid_state_and_rejects_csrf_mismatch() {
        let server = LoopbackCallbackServer::bind().expect("bind loopback");
        let redirect = server.redirect_uri();
        assert!(redirect.starts_with("http://127.0.0.1:"));

        // First test CSRF state mismatch.
        let bad_url = format!("{redirect}?code=abc&state=wrong-state");
        let handle = std::thread::spawn(move || {
            let _ = reqwest::blocking::get(bad_url);
        });
        let err = server
            .wait_for_code("expected-state", Duration::from_secs(5), None)
            .expect_err("mismatched state must fail");
        assert!(matches!(err, McpError::OAuthStateMismatch));
        let _ = handle.join();

        // Now test valid state + code.
        let good_url = format!("{redirect}?code=auth-code-99&state=expected-state");
        let handle = std::thread::spawn(move || {
            let _ = reqwest::blocking::get(good_url);
        });
        let code = server
            .wait_for_code("expected-state", Duration::from_secs(5), None)
            .expect("valid callback succeeds");
        assert_eq!(code, "auth-code-99");
        let _ = handle.join();
    }

    #[test]
    fn loopback_callback_enforces_timeout() {
        let server = LoopbackCallbackServer::bind().expect("bind loopback");
        let err = server
            .wait_for_code("state-x", Duration::from_millis(80), None)
            .expect_err("must time out");
        assert!(matches!(err, McpError::OAuthTimeout));
    }

    #[test]
    fn oauth_code_exchange_and_refresh_store_only_in_keyring() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock token server");
        let port = listener.local_addr().unwrap().port();
        let server_thread = std::thread::spawn(move || {
            for idx in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if idx == 0 {
                    assert!(req.contains("grant_type=authorization_code"));
                    assert!(req.contains("code_verifier=verifier-xyz"));
                    r#"{"access_token":"tok-initial-secret","refresh_token":"ref-secret-1","expires_in":3600,"token_type":"Bearer"}"#
                } else {
                    assert!(req.contains("grant_type=refresh_token"));
                    assert!(req.contains("refresh_token=ref-secret-1"));
                    r#"{"access_token":"tok-refreshed-secret","expires_in":7200,"token_type":"Bearer"}"#
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
        let oauth = McpOAuthConfig {
            authorization_url: format!("http://127.0.0.1:{port}/authorize"),
            token_url: format!("http://127.0.0.1:{port}/token"),
            client_id: "nexora-test-client".to_string(),
            scopes: vec!["mcp:tools".to_string()],
        };

        let status = exchange_code_and_store_with(
            &keyring,
            "remote-srv",
            &oauth,
            "code-123",
            "http://127.0.0.1:9999/callback",
            "verifier-xyz",
            Duration::from_secs(5),
        )
        .expect("token exchange succeeds");
        assert!(status.has_auth);
        assert_eq!(status.auth_kind, "oauth");
        assert!(status.expires_at.is_some());
        // Debug format of StoredMcpAuth never leaks tokens.
        let stored = McpCredentialStore::load_record_with(&keyring, "remote-srv")
            .unwrap()
            .unwrap();
        assert_eq!(stored.access_token, "tok-initial-secret");
        assert!(!format!("{stored:?}").contains("tok-initial-secret"));
        assert!(!format!("{stored:?}").contains("ref-secret-1"));

        let refreshed_status =
            refresh_oauth_token_with(&keyring, "remote-srv", &oauth, Duration::from_secs(5))
                .expect("refresh succeeds");
        assert!(refreshed_status.has_auth);
        let updated = McpCredentialStore::load_record_with(&keyring, "remote-srv")
            .unwrap()
            .unwrap();
        assert_eq!(updated.access_token, "tok-refreshed-secret");
        assert_eq!(updated.refresh_token.as_deref(), Some("ref-secret-1"));

        server_thread.join().unwrap();
    }
}
