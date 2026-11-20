//! Tauri IPC commands for user-added MCP servers, tool discovery, and OAuth
//! 2.1 + PKCE sign-in (`M18` / `P26`).
//!
//! Thin IPC translation only (ARCHITECTURE.md §5):
//! - Delegates all validation, transport, OAuth, and persistence to
//!   [`crate::application::mcp`].
//! - Maps failures into secret-free [`CommandError`] values (never includes
//!   tokens, credentials, env values, or raw bodies).
//! - Blocking network/process commands execute on Tauri's blocking pool via
//!   `spawn_blocking`.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::application::agent::control::CancellationToken;
use crate::application::import::McpServerEntry;
use crate::application::mcp::{
    run_oauth_login_flow, McpAuthStatus, McpError, McpServerView, McpService, McpToolSummary,
    DEFAULT_OAUTH_CALLBACK_TIMEOUT,
};
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};

/// List all user-added MCP servers with secret-free authentication status.
/// Zero bundled servers exist by default.
#[tauri::command]
pub(crate) fn mcp_list_servers(
    db: State<'_, Database>,
) -> Result<Vec<McpServerView>, CommandError> {
    McpService::new(db.inner())
        .list_servers()
        .map_err(Into::into)
}

/// Add or update one user-added MCP server (`stdio` or `streamable_http`) in
/// Personal settings (Invariant 4: never project-defined, never auto-approved).
#[tauri::command]
pub(crate) fn mcp_upsert_server(
    server: McpServerEntry,
    db: State<'_, Database>,
) -> Result<Vec<McpServerView>, CommandError> {
    McpService::new(db.inner())
        .upsert_server(server)
        .map_err(Into::into)
}

/// Remove a user-added MCP server and delete its keyring credentials.
#[tauri::command]
pub(crate) fn mcp_remove_server(
    name: String,
    db: State<'_, Database>,
) -> Result<Vec<McpServerView>, CommandError> {
    McpService::new(db.inner())
        .remove_server(&name)
        .map_err(Into::into)
}

/// Toggle `enabled` on a configured user-added MCP server.
#[tauri::command]
pub(crate) fn mcp_set_server_enabled(
    name: String,
    enabled: bool,
    db: State<'_, Database>,
) -> Result<Vec<McpServerView>, CommandError> {
    McpService::new(db.inner())
        .set_server_enabled(&name, enabled)
        .map_err(Into::into)
}

/// Connect to a configured MCP server and return its discovered tools
/// (namespaced as `mcp__<server>__<tool>`).
#[tauri::command]
pub(crate) async fn mcp_list_server_tools(
    name: String,
    app: AppHandle,
) -> Result<Vec<McpToolSummary>, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let token = CancellationToken::new();
        McpService::new(db.inner())
            .list_server_tools(&name, &token)
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("mcp_list_server_tools blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "MCP tool discovery could not be completed",
            ))
        }
    }
}

/// Store a Bearer token in the OS secure keyring for a `streamable_http` MCP
/// server (never in `SQLite` or logs).
#[tauri::command]
pub(crate) fn mcp_set_bearer_token(
    name: String,
    token: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    McpService::new(db.inner())
        .set_bearer_token(&name, &token)
        .map_err(Into::into)
}

/// Remove stored OAuth / Bearer credentials from the OS keyring for `name`.
#[tauri::command]
pub(crate) fn mcp_clear_auth(name: String, db: State<'_, Database>) -> Result<(), CommandError> {
    McpService::new(db.inner())
        .clear_auth(&name)
        .map_err(Into::into)
}

/// Start the OAuth 2.1 + PKCE (`S256`) sign-in flow for a configured
/// `streamable_http` MCP server: binds loopback `127.0.0.1:<port>`, opens the
/// validated authorization URL in the user's browser, verifies CSRF `state`,
/// exchanges the authorization code, and stores tokens exclusively in the OS
/// keyring.
#[tauri::command]
pub(crate) async fn mcp_start_oauth(
    name: String,
    app: AppHandle,
) -> Result<McpAuthStatus, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        run_oauth_login_flow(
            db.inner(),
            &name,
            |auth_url| {
                handle
                    .opener()
                    .open_url(auth_url, None::<&str>)
                    .map_err(|_| McpError::OAuthFailed("could not open browser for OAuth sign-in"))
            },
            DEFAULT_OAUTH_CALLBACK_TIMEOUT,
        )
        .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("mcp_start_oauth blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "MCP OAuth sign-in could not be completed",
            ))
        }
    }
}

/// Refresh stored OAuth 2.1 tokens in the OS keyring for a configured
/// `streamable_http` MCP server.
#[tauri::command]
pub(crate) async fn mcp_refresh_oauth(
    name: String,
    app: AppHandle,
) -> Result<McpAuthStatus, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        McpService::new(db.inner())
            .refresh_oauth(&name)
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("mcp_refresh_oauth blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "MCP OAuth token refresh could not be completed",
            ))
        }
    }
}
