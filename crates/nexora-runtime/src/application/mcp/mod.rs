//! User-added MCP (Model Context Protocol) transports, OAuth 2.1 + PKCE,
//! tool discovery, and agent runtime integration (`M18` / `P26`).
//!
//! Hard invariants:
//! - Zero bundled MCP servers: `list_servers` returns empty until the user
//!   explicitly adds a server.
//! - Personal scope only (Invariant 4): MCP servers are never loaded from or
//!   overridden by untrusted workspace `.nexora/` configuration.
//! - Never auto-approved: every MCP tool call is namespaced as
//!   `mcp__<server>__<tool>`, classified as `RiskClass::ExternalMcp`, gated
//!   through `M04` `permissions.rs` + `ApprovalGate`, and wrapped in
//!   `injection.rs` untrusted envelopes + marker scans.
//! - Secret safety: OAuth tokens and Bearer credentials live exclusively in
//!   the OS secure keyring (`keyring` crate) — never in `SQLite`, IPC payloads,
//!   or logs.

pub(crate) mod http;
pub(crate) mod jsonrpc;
pub(crate) mod oauth;
pub(crate) mod stdio;
pub(crate) mod url_safety;

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::application::agent::control::CancellationToken;
use crate::application::execution::ToolDefinition;
use crate::application::import::{
    McpOAuthConfig, McpServerEntry, McpServerList, McpTransportKind, MCP_SERVERS_KEY,
};
use crate::application::settings::SettingsService;
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::providers::credentials::{CredentialBackend, KeyringBackend};

pub(crate) use jsonrpc::{descriptor_to_tool_definition, parse_mcp_tool_name, McpToolDescriptor};
pub use oauth::{
    build_authorization_url, exchange_code_and_store_with, generate_csrf_state, generate_pkce,
    refresh_oauth_token_with, resolve_bearer_token_with, LoopbackCallbackServer, McpAuthStatus,
    McpCredentialStore, DEFAULT_OAUTH_CALLBACK_TIMEOUT, DEFAULT_OAUTH_HTTP_TIMEOUT,
};

use self::http::{StreamableHttpClient, DEFAULT_HTTP_TIMEOUT};
use self::stdio::{StdioMcpClient, DEFAULT_STDIO_TIMEOUT};

/// Secret-free MCP application error taxonomy.
/// Never carries tokens, credentials, env values, URLs with secrets, or raw
/// server payloads.
#[derive(Debug)]
pub enum McpError {
    NotFound(String),
    InvalidConfig(&'static str),
    InvalidUrl(&'static str),
    Transport(&'static str),
    Protocol(&'static str),
    JsonRpcError { code: i64 },
    HttpStatus { status: u16 },
    Unauthorized,
    Timeout,
    Cancelled,
    ResponseTooLarge,
    KeyringUnavailable,
    OAuthStateMismatch,
    OAuthTimeout,
    OAuthFailed(&'static str),
    Database(DatabaseError),
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "MCP server '{name}' is not configured"),
            Self::InvalidConfig(msg)
            | Self::InvalidUrl(msg)
            | Self::Transport(msg)
            | Self::Protocol(msg)
            | Self::OAuthFailed(msg) => write!(f, "{msg}"),
            Self::JsonRpcError { code } => {
                write!(f, "MCP server returned JSON-RPC error (code {code})")
            }
            Self::HttpStatus { status } => {
                write!(f, "MCP server returned HTTP status {status}")
            }
            Self::Unauthorized => {
                write!(
                    f,
                    "MCP server rejected credentials (401 Unauthorized); authenticate in Settings"
                )
            }
            Self::Timeout => write!(f, "MCP server request timed out"),
            Self::Cancelled => write!(f, "MCP server request was cancelled"),
            Self::ResponseTooLarge => {
                write!(f, "MCP server response exceeded the maximum size limit")
            }
            Self::KeyringUnavailable => {
                write!(f, "the operating-system keyring is unavailable")
            }
            Self::OAuthStateMismatch => {
                write!(f, "OAuth CSRF state verification failed")
            }
            Self::OAuthTimeout => write!(f, "OAuth sign-in timed out waiting for callback"),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for McpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            _ => None,
        }
    }
}

impl From<DatabaseError> for McpError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

/// Secret-free view of a configured user-added MCP server for IPC / UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpServerView {
    pub name: String,
    pub transport: McpTransportKind,
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub url: Option<String>,
    pub oauth: Option<McpOAuthConfig>,
    pub has_auth: bool,
    pub auth_kind: &'static str,
    pub expires_at: Option<i64>,
}

impl McpServerView {
    fn from_entry(entry: McpServerEntry, auth: McpAuthStatus) -> Self {
        Self {
            name: entry.name,
            transport: entry.transport,
            enabled: entry.enabled,
            command: entry.command,
            args: entry.args,
            env: entry.env,
            url: entry.url,
            oauth: entry.oauth,
            has_auth: auth.has_auth,
            auth_kind: auth.auth_kind,
            expires_at: auth.expires_at,
        }
    }
}

/// Secret-free summary of one discovered MCP tool for IPC / UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpToolSummary {
    pub namespaced_name: String,
    pub server_name: String,
    pub tool_name: String,
    pub description: String,
}

/// Application-layer service managing user-added MCP servers, credentials
/// (keyring-only), discovery, and tool execution.
///
/// Ships with ZERO bundled servers. Always reads personal `MCP_SERVERS_KEY`
/// directly from `SQLite` `app_settings` (never project `.nexora/` config).
pub struct McpService<'a> {
    db: &'a Database,
}

impl<'a> McpService<'a> {
    #[must_use]
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Load the validated personal MCP server list (empty by default; zero
    /// bundled servers).
    pub(crate) fn load_list(&self) -> Result<McpServerList, McpError> {
        let Some(raw) = SettingsService::new(self.db).read(MCP_SERVERS_KEY)? else {
            return Ok(McpServerList::default());
        };
        McpServerList::from_json(&raw)
            .map_err(|_| McpError::InvalidConfig("stored MCP server list is invalid"))
    }

    fn save_list(&self, list: &McpServerList) -> Result<(), McpError> {
        let json = list
            .to_json()
            .map_err(|_| McpError::InvalidConfig("MCP server configuration failed validation"))?;
        SettingsService::new(self.db).write(MCP_SERVERS_KEY, Some(&json))?;
        Ok(())
    }

    /// List all configured user-added MCP servers with secret-free auth status.
    pub fn list_servers(&self) -> Result<Vec<McpServerView>, McpError> {
        self.list_servers_with(&KeyringBackend)
    }

    pub(crate) fn list_servers_with(
        &self,
        backend: &dyn CredentialBackend,
    ) -> Result<Vec<McpServerView>, McpError> {
        let list = self.load_list()?;
        Ok(list
            .servers
            .into_iter()
            .map(|entry| {
                let auth = McpCredentialStore::status_with(backend, &entry.name);
                McpServerView::from_entry(entry, auth)
            })
            .collect())
    }

    /// Add or update a user-added MCP server in Personal settings.
    ///
    /// When overwriting an existing server by `name`, clears stored OS keyring
    /// credentials whenever `url`, `transport`, or `oauth` changed so reusing a
    /// server name for a new endpoint never leaks the previous token.
    pub fn upsert_server(&self, entry: McpServerEntry) -> Result<Vec<McpServerView>, McpError> {
        self.upsert_server_with(&KeyringBackend, entry)
    }

    pub(crate) fn upsert_server_with(
        &self,
        backend: &dyn CredentialBackend,
        entry: McpServerEntry,
    ) -> Result<Vec<McpServerView>, McpError> {
        entry.validate_entry().map_err(McpError::InvalidConfig)?;
        let mut list = self.load_list()?;
        let mut clear_credentials_for: Option<String> = None;
        if let Some(existing) = list.servers.iter_mut().find(|s| s.name == entry.name) {
            if existing.url != entry.url
                || existing.transport != entry.transport
                || existing.oauth != entry.oauth
            {
                clear_credentials_for = Some(entry.name.clone());
            }
            *existing = entry;
        } else {
            list.servers.push(entry);
        }
        let json = list
            .to_json()
            .map_err(|_| McpError::InvalidConfig("MCP server configuration failed validation"))?;
        if let Some(name) = clear_credentials_for {
            if let Err(err) = McpCredentialStore::remove_with(backend, &name) {
                if McpCredentialStore::status_with(backend, &name).has_auth {
                    return Err(err);
                }
            }
        }
        SettingsService::new(self.db).write(MCP_SERVERS_KEY, Some(&json))?;
        self.list_servers_with(backend)
    }

    /// Remove a user-added MCP server and delete its keyring credentials.
    pub fn remove_server(&self, name: &str) -> Result<Vec<McpServerView>, McpError> {
        let mut list = self.load_list()?;
        let before = list.servers.len();
        list.servers.retain(|s| s.name != name);
        if list.servers.len() == before {
            return Err(McpError::NotFound(name.to_string()));
        }
        self.save_list(&list)?;
        let _ = McpCredentialStore::remove(name);
        self.list_servers()
    }

    /// Toggle `enabled` on an existing user-added MCP server.
    pub fn set_server_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Vec<McpServerView>, McpError> {
        let mut list = self.load_list()?;
        let Some(server) = list.servers.iter_mut().find(|s| s.name == name) else {
            return Err(McpError::NotFound(name.to_string()));
        };
        server.enabled = enabled;
        self.save_list(&list)?;
        self.list_servers()
    }

    /// Find one configured server by exact name.
    pub(crate) fn find_server(&self, name: &str) -> Result<McpServerEntry, McpError> {
        let list = self.load_list()?;
        list.servers
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| McpError::NotFound(name.to_string()))
    }

    /// Store a Bearer token in the OS keyring for a configured `streamable_http`
    /// server (never in `SQLite`).
    pub fn set_bearer_token(&self, name: &str, token: &str) -> Result<(), McpError> {
        let server = self.find_server(name)?;
        if server.transport != McpTransportKind::StreamableHttp {
            return Err(McpError::InvalidConfig(
                "bearer authentication applies only to streamable_http MCP servers",
            ));
        }
        McpCredentialStore::store_bearer(name, token)
    }

    /// Remove stored keyring credentials for `name`.
    pub fn clear_auth(&self, name: &str) -> Result<(), McpError> {
        let _ = self.find_server(name)?;
        McpCredentialStore::remove(name)
    }

    /// Refresh an OAuth 2.1 token for a configured `streamable_http` server.
    pub fn refresh_oauth(&self, name: &str) -> Result<McpAuthStatus, McpError> {
        let server = self.find_server(name)?;
        if server.transport != McpTransportKind::StreamableHttp {
            return Err(McpError::InvalidConfig(
                "OAuth applies only to streamable_http MCP servers",
            ));
        }
        let oauth = server.oauth.as_ref().ok_or(McpError::InvalidConfig(
            "MCP server has no OAuth configuration",
        ))?;
        refresh_oauth_token_with(&KeyringBackend, name, oauth, DEFAULT_OAUTH_HTTP_TIMEOUT)
    }

    /// Discover tools exposed by a single configured server.
    pub fn list_server_tools(
        &self,
        name: &str,
        token: &CancellationToken,
    ) -> Result<Vec<McpToolSummary>, McpError> {
        let server = self.find_server(name)?;
        let descriptors = discover_tools_for_entry(&server, token)?;
        Ok(descriptors
            .iter()
            .filter_map(|desc| {
                let def = descriptor_to_tool_definition(&server.name, desc)?;
                Some(McpToolSummary {
                    namespaced_name: def.name,
                    server_name: server.name.clone(),
                    tool_name: desc.name.clone(),
                    description: def.description,
                })
            })
            .collect())
    }
}

/// Discover MCP tools from one server entry via its configured transport.
pub(crate) fn discover_tools_for_entry(
    entry: &McpServerEntry,
    token: &CancellationToken,
) -> Result<Vec<McpToolDescriptor>, McpError> {
    match entry.transport {
        McpTransportKind::Stdio => {
            let mut client = StdioMcpClient::connect(
                &entry.command,
                &entry.args,
                &entry.env,
                DEFAULT_STDIO_TIMEOUT,
                token,
            )?;
            client.list_tools(DEFAULT_STDIO_TIMEOUT, token)
        }
        McpTransportKind::StreamableHttp => {
            let url = entry
                .url
                .as_deref()
                .ok_or(McpError::InvalidConfig("MCP HTTP server is missing url"))?;
            let bearer = resolve_bearer_token_with(
                &KeyringBackend,
                &entry.name,
                entry.oauth.as_ref(),
                DEFAULT_OAUTH_HTTP_TIMEOUT,
            )?;
            match StreamableHttpClient::connect(url, bearer, DEFAULT_HTTP_TIMEOUT, token)
                .and_then(|mut c| c.list_tools(token))
            {
                Ok(tools) => Ok(tools),
                Err(McpError::Unauthorized) => {
                    if let Some(oauth) = entry.oauth.as_ref() {
                        refresh_oauth_token_with(
                            &KeyringBackend,
                            &entry.name,
                            oauth,
                            DEFAULT_OAUTH_HTTP_TIMEOUT,
                        )?;
                        let fresh = resolve_bearer_token_with(
                            &KeyringBackend,
                            &entry.name,
                            Some(oauth),
                            DEFAULT_OAUTH_HTTP_TIMEOUT,
                        )?;
                        let mut retry =
                            StreamableHttpClient::connect(url, fresh, DEFAULT_HTTP_TIMEOUT, token)?;
                        return retry.list_tools(token);
                    }
                    Err(McpError::Unauthorized)
                }
                Err(err) => Err(err),
            }
        }
    }
}

/// Execute one MCP tool on `entry` via its configured transport.
pub(crate) fn call_tool_for_entry(
    entry: &McpServerEntry,
    tool_name: &str,
    arguments: &Value,
    token: &CancellationToken,
) -> Result<(String, bool), McpError> {
    match entry.transport {
        McpTransportKind::Stdio => {
            let mut client = StdioMcpClient::connect(
                &entry.command,
                &entry.args,
                &entry.env,
                DEFAULT_STDIO_TIMEOUT,
                token,
            )?;
            client.call_tool(tool_name, arguments, DEFAULT_STDIO_TIMEOUT, token)
        }
        McpTransportKind::StreamableHttp => {
            let url = entry
                .url
                .as_deref()
                .ok_or(McpError::InvalidConfig("MCP HTTP server is missing url"))?;
            let bearer = resolve_bearer_token_with(
                &KeyringBackend,
                &entry.name,
                entry.oauth.as_ref(),
                DEFAULT_OAUTH_HTTP_TIMEOUT,
            )?;
            match StreamableHttpClient::connect(url, bearer, DEFAULT_HTTP_TIMEOUT, token)
                .and_then(|mut c| c.call_tool(tool_name, arguments, token))
            {
                Ok(res) => Ok(res),
                Err(McpError::Unauthorized) => {
                    if let Some(oauth) = entry.oauth.as_ref() {
                        refresh_oauth_token_with(
                            &KeyringBackend,
                            &entry.name,
                            oauth,
                            DEFAULT_OAUTH_HTTP_TIMEOUT,
                        )?;
                        let fresh = resolve_bearer_token_with(
                            &KeyringBackend,
                            &entry.name,
                            Some(oauth),
                            DEFAULT_OAUTH_HTTP_TIMEOUT,
                        )?;
                        let mut retry =
                            StreamableHttpClient::connect(url, fresh, DEFAULT_HTTP_TIMEOUT, token)?;
                        return retry.call_tool(tool_name, arguments, token);
                    }
                    Err(McpError::Unauthorized)
                }
                Err(err) => Err(err),
            }
        }
    }
}

/// Seam injected into [`crate::application::agent::runner::AgentRunner`] for
/// MCP tool discovery and execution (allows deterministic unit testing of
/// `M04` approval/permission/injection gating without spawning processes).
pub(crate) trait McpToolRuntime: Send + Sync {
    /// Namespaced (`mcp__<server>__<tool>`) tool definitions exposed to the model.
    fn definitions(&self) -> Vec<ToolDefinition>;

    /// Whether `namespaced_name` is registered on an enabled server in this run.
    fn has_tool(&self, namespaced_name: &str) -> bool;

    /// Invoke `namespaced_name` with raw JSON `arguments_json`.
    fn call_tool(
        &self,
        namespaced_name: &str,
        arguments_json: &str,
        token: &CancellationToken,
    ) -> Result<String, String>;
}

/// Production run-scoped MCP runtime discovered from enabled personal servers.
pub(crate) struct LiveMcpRuntime {
    definitions: Vec<ToolDefinition>,
    dispatch_map: HashMap<String, (McpServerEntry, String)>,
}

impl LiveMcpRuntime {
    /// Best-effort discovery across all enabled personal MCP servers at run
    /// start. Unreachable servers are logged (secret-free) and skipped so a
    /// broken external server never prevents an agent run from starting.
    #[must_use]
    pub(crate) fn discover(db: &Database, token: &CancellationToken) -> Option<Self> {
        let list = match McpService::new(db).load_list() {
            Ok(list) => list,
            Err(err) => {
                log::warn!("mcp discovery skipped: {err}");
                return None;
            }
        };
        let enabled: Vec<McpServerEntry> = list.servers.into_iter().filter(|s| s.enabled).collect();
        if enabled.is_empty() {
            return None;
        }

        let mut definitions = Vec::new();
        let mut dispatch_map = HashMap::new();
        for server in enabled {
            if token.is_cancelled() {
                break;
            }
            match discover_tools_for_entry(&server, token) {
                Ok(descriptors) => {
                    for desc in descriptors {
                        if let Some(def) = descriptor_to_tool_definition(&server.name, &desc) {
                            if !dispatch_map.contains_key(&def.name) {
                                dispatch_map.insert(def.name.clone(), (server.clone(), desc.name));
                                definitions.push(def);
                            }
                        }
                    }
                }
                Err(err) => {
                    log::warn!("mcp discovery failed for server '{}': {err}", server.name);
                }
            }
        }

        if definitions.is_empty() {
            None
        } else {
            Some(Self {
                definitions,
                dispatch_map,
            })
        }
    }
}

impl McpToolRuntime for LiveMcpRuntime {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }

    fn has_tool(&self, namespaced_name: &str) -> bool {
        self.dispatch_map.contains_key(namespaced_name)
    }

    fn call_tool(
        &self,
        namespaced_name: &str,
        arguments_json: &str,
        token: &CancellationToken,
    ) -> Result<String, String> {
        let Some((entry, raw_tool_name)) = self.dispatch_map.get(namespaced_name) else {
            return Err(format!("unknown MCP tool '{namespaced_name}'"));
        };
        let args: Value = serde_json::from_str(arguments_json)
            .map_err(|_| "MCP tool arguments must be valid JSON".to_string())?;
        if !args.is_object() {
            return Err("MCP tool arguments must be a JSON object".to_string());
        }
        let (output, is_error) =
            call_tool_for_entry(entry, raw_tool_name, &args, token).map_err(|e| e.to_string())?;
        if is_error {
            Err(output)
        } else {
            Ok(output)
        }
    }
}

/// Complete OAuth 2.1 + PKCE sign-in flow:
/// 1. Binds ephemeral loopback callback `127.0.0.1:<port>`.
/// 2. Generates `S256` PKCE pair and CSRF `state`.
/// 3. Builds validated authorization URL and invokes `open_browser(url)`.
/// 4. Waits for callback with timeout, verifies `state`, exchanges code for
///    tokens, and persists tokens in OS keyring ONLY.
pub fn run_oauth_login_flow(
    db: &Database,
    server_name: &str,
    open_browser: impl FnOnce(&str) -> Result<(), McpError>,
    callback_timeout: Duration,
) -> Result<McpAuthStatus, McpError> {
    let server = McpService::new(db).find_server(server_name)?;
    if server.transport != McpTransportKind::StreamableHttp {
        return Err(McpError::InvalidConfig(
            "OAuth applies only to streamable_http MCP servers",
        ));
    }
    let oauth = server.oauth.as_ref().ok_or(McpError::InvalidConfig(
        "MCP server has no OAuth configuration",
    ))?;

    let listener = LoopbackCallbackServer::bind()?;
    let redirect_uri = listener.redirect_uri();
    let pkce = generate_pkce();
    let state = generate_csrf_state();
    let auth_url = build_authorization_url(oauth, &redirect_uri, &pkce, &state)?;

    open_browser(&auth_url)?;

    let code = listener.wait_for_code(&state, callback_timeout, None)?;
    exchange_code_and_store_with(
        &KeyringBackend,
        server_name,
        oauth,
        &code,
        &redirect_uri,
        &pkce.verifier,
        DEFAULT_OAUTH_HTTP_TIMEOUT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;
    use std::collections::BTreeMap;

    #[test]
    fn zero_bundled_mcp_servers_by_default() {
        let db = in_memory_database();
        let svc = McpService::new(&db);
        assert!(svc.load_list().unwrap().servers.is_empty());
        assert!(svc.list_servers().unwrap().is_empty());
    }

    #[test]
    fn crud_supports_stdio_and_streamable_http_and_toggle() {
        let db = in_memory_database();
        let svc = McpService::new(&db);

        let stdio_entry = McpServerEntry {
            name: "local-fs".to_string(),
            transport: McpTransportKind::Stdio,
            enabled: true,
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "demo-mcp".to_string()],
            env: BTreeMap::new(),
            url: None,
            oauth: None,
        };
        let list = svc.upsert_server(stdio_entry).expect("add stdio");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "local-fs");
        assert!(list[0].enabled);

        let http_entry = McpServerEntry {
            name: "remote-api".to_string(),
            transport: McpTransportKind::StreamableHttp,
            enabled: true,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some("https://mcp.example.com/v1".to_string()),
            oauth: Some(McpOAuthConfig {
                authorization_url: "https://auth.example.com/authorize".to_string(),
                token_url: "https://auth.example.com/token".to_string(),
                client_id: "nexora-desktop".to_string(),
                scopes: vec!["tools:read".to_string()],
            }),
        };
        let list = svc.upsert_server(http_entry).expect("add http");
        assert_eq!(list.len(), 2);

        let toggled = svc
            .set_server_enabled("remote-api", false)
            .expect("toggle off");
        let remote = toggled.iter().find(|s| s.name == "remote-api").unwrap();
        assert!(!remote.enabled);

        let after_rm = svc.remove_server("local-fs").expect("remove");
        assert_eq!(after_rm.len(), 1);
        assert_eq!(after_rm[0].name, "remote-api");
    }

    #[test]
    fn rejects_ssrf_http_server_on_upsert() {
        let db = in_memory_database();
        let svc = McpService::new(&db);
        let bad = McpServerEntry {
            name: "ssrf".to_string(),
            transport: McpTransportKind::StreamableHttp,
            enabled: true,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some("http://169.254.169.254/latest/meta-data".to_string()),
            oauth: None,
        };
        assert!(svc.upsert_server(bad).is_err());
    }

    #[derive(Default)]
    struct MemoryKeyring {
        entries: std::sync::Mutex<HashMap<(String, String), String>>,
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

    fn sample_http_entry(name: &str, url: &str, oauth: Option<McpOAuthConfig>) -> McpServerEntry {
        McpServerEntry {
            name: name.to_string(),
            transport: McpTransportKind::StreamableHttp,
            enabled: true,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some(url.to_string()),
            oauth,
        }
    }

    fn sample_oauth(client_id: &str) -> McpOAuthConfig {
        McpOAuthConfig {
            authorization_url: "https://auth.example.com/authorize".to_string(),
            token_url: "https://auth.example.com/token".to_string(),
            client_id: client_id.to_string(),
            scopes: vec!["tools:read".to_string()],
        }
    }

    #[test]
    fn upsert_clears_credentials_when_url_transport_or_oauth_changes() {
        let db = in_memory_database();
        let svc = McpService::new(&db);
        let keyring = MemoryKeyring::default();

        let original = sample_http_entry("remote-api", "https://one.example.com/mcp", None);
        svc.upsert_server_with(&keyring, original.clone())
            .expect("insert initial server");
        McpCredentialStore::store_bearer_with(&keyring, "remote-api", "tok-initial")
            .expect("store bearer");

        // Updating non-auth metadata (enabled) keeps the stored credential intact.
        let mut toggled = original;
        toggled.enabled = false;
        let views = svc
            .upsert_server_with(&keyring, toggled)
            .expect("toggle enabled via upsert");
        assert!(views[0].has_auth);
        assert_eq!(
            McpCredentialStore::load_record_with(&keyring, "remote-api")
                .unwrap()
                .map(|r| r.access_token),
            Some("tok-initial".to_string())
        );

        // Changing endpoint URL clears stored credentials so the old token cannot leak.
        let moved_url = sample_http_entry("remote-api", "https://two.example.com/mcp", None);
        let views = svc
            .upsert_server_with(&keyring, moved_url)
            .expect("update url");
        assert!(!views[0].has_auth);
        assert!(McpCredentialStore::load_record_with(&keyring, "remote-api")
            .unwrap()
            .is_none());

        // Changing OAuth config (None -> Some, or different client_id) clears stored credentials.
        McpCredentialStore::store_bearer_with(&keyring, "remote-api", "tok-before-oauth")
            .expect("store bearer");
        let with_oauth = sample_http_entry(
            "remote-api",
            "https://two.example.com/mcp",
            Some(sample_oauth("client-a")),
        );
        let views = svc
            .upsert_server_with(&keyring, with_oauth)
            .expect("add oauth");
        assert!(!views[0].has_auth);
        assert!(McpCredentialStore::load_record_with(&keyring, "remote-api")
            .unwrap()
            .is_none());

        McpCredentialStore::store_bearer_with(&keyring, "remote-api", "tok-client-a")
            .expect("store bearer");
        let changed_oauth = sample_http_entry(
            "remote-api",
            "https://two.example.com/mcp",
            Some(sample_oauth("client-b")),
        );
        let views = svc
            .upsert_server_with(&keyring, changed_oauth)
            .expect("change oauth client");
        assert!(!views[0].has_auth);
        assert!(McpCredentialStore::load_record_with(&keyring, "remote-api")
            .unwrap()
            .is_none());

        // Changing transport (StreamableHttp -> Stdio) clears stored credentials.
        McpCredentialStore::store_bearer_with(&keyring, "remote-api", "tok-before-stdio")
            .expect("store bearer");
        let switched_transport = McpServerEntry {
            name: "remote-api".to_string(),
            transport: McpTransportKind::Stdio,
            enabled: true,
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "demo-mcp".to_string()],
            env: BTreeMap::new(),
            url: None,
            oauth: None,
        };
        let views = svc
            .upsert_server_with(&keyring, switched_transport)
            .expect("switch to stdio");
        assert!(!views[0].has_auth);
        assert!(McpCredentialStore::load_record_with(&keyring, "remote-api")
            .unwrap()
            .is_none());
    }

    #[test]
    fn upsert_rejects_non_identifier_and_oversized_server_names() {
        let db = in_memory_database();
        let svc = McpService::new(&db);
        let keyring = MemoryKeyring::default();
        let too_long = "a".repeat(jsonrpc::MAX_MCP_SERVER_NAME_LEN + 1);
        for bad_name in [
            "",
            "my.server",
            "org/repo",
            "srv:1",
            "bad__srv",
            "bad name",
            too_long.as_str(),
        ] {
            let entry = McpServerEntry {
                name: bad_name.to_string(),
                transport: McpTransportKind::Stdio,
                enabled: true,
                command: "npx".to_string(),
                args: Vec::new(),
                env: BTreeMap::new(),
                url: None,
                oauth: None,
            };
            assert!(
                svc.upsert_server_with(&keyring, entry).is_err(),
                "expected invalid server name to be rejected: {bad_name:?}"
            );
        }

        let max_valid = "s".repeat(jsonrpc::MAX_MCP_SERVER_NAME_LEN);
        let entry = McpServerEntry {
            name: max_valid.clone(),
            transport: McpTransportKind::Stdio,
            enabled: true,
            command: "npx".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            oauth: None,
        };
        svc.upsert_server_with(&keyring, entry)
            .expect("max-length identifier server name accepted");
        assert!(jsonrpc::format_mcp_tool_name(&max_valid, "ping").is_some());
    }
}
