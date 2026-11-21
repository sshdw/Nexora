//! Tauri commands over the existing [`CredentialStore`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! Provider credentials live exclusively in the OS secure keyring
//! (ARCHITECTURE.md В§12; DATABASE.md В§14) and are never written to `SQLite`.
//! These commands delegate to the existing store and expose only safe
//! operations:
//!
//! - write a credential (`add` / `update`, both keyring upserts),
//! - remove a credential,
//! - check a credential's *presence* (never its value).
//!
//! There is deliberately **no** command that returns a credential value: the
//! secret is unnecessary for the UI and must not be returned unnecessarily
//! (FR-014; acceptance criterion #6).

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::application::providers::{ProviderError, ProviderService};
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::providers::chatgpt_oauth::{
    refresh_tokens, run_oauth_login_flow, ChatGptOAuthError, ChatGptOAuthStatus, ChatGptOAuthStore,
    DEFAULT_OAUTH_CALLBACK_TIMEOUT,
};
use crate::infrastructure::providers::credentials::CredentialStore;
use crate::infrastructure::providers::openai::{
    PROVIDER_DISPLAY_NAME as OPENAI_DISPLAY_NAME, PROVIDER_NAME as OPENAI_PROVIDER_NAME,
};

use super::error::{CommandError, ErrorKind};

fn is_provider_already_exists(err: &ProviderError) -> bool {
    matches!(
        err,
        ProviderError::Database(DatabaseError::Sqlite(rusqlite::Error::SqliteFailure(
            sqlite_err,
            _
        ))) if sqlite_err.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn ensure_openai_provider_row_or_rollback<R>(
    service: &ProviderService<'_>,
    rollback_oauth: R,
) -> Result<(), CommandError>
where
    R: FnOnce(),
{
    let result = match service.read_by_name(OPENAI_PROVIDER_NAME) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => match service.create(OPENAI_PROVIDER_NAME, OPENAI_DISPLAY_NAME) {
            Ok(_) => Ok(()),
            Err(err) if is_provider_already_exists(&err) => Ok(()),
            Err(err) => Err(CommandError::from(err)),
        },
        Err(err) => Err(CommandError::from(err)),
    };
    if result.is_err() {
        rollback_oauth();
    }
    result
}

/// Store a new credential for `provider` in the OS secure keyring (FR-014).
#[tauri::command]
pub(crate) fn add_provider_credential(
    provider: String,
    credential: String,
) -> Result<(), CommandError> {
    CredentialStore::add(&provider, &credential).map_err(Into::into)
}

/// Update the stored credential for `provider` in the OS secure keyring
/// (FR-014).
#[tauri::command]
pub(crate) fn update_provider_credential(
    provider: String,
    credential: String,
) -> Result<(), CommandError> {
    CredentialStore::update(&provider, &credential).map_err(Into::into)
}

/// Remove the stored credential for `provider` (no-op when none exists).
#[tauri::command]
pub(crate) fn remove_provider_credential(provider: String) -> Result<(), CommandError> {
    CredentialStore::remove(&provider).map_err(Into::into)
}

/// Report whether `provider` has a stored credential — presence only, never
/// the secret value.
#[tauri::command]
pub(crate) fn has_provider_credential(provider: String) -> Result<bool, CommandError> {
    CredentialStore::exists(&provider).map_err(Into::into)
}

/// Report secret-free opt-in `ChatGPT` Plus/Pro OAuth connection status from the
/// OS secure keyring (`S11-11a`). Never returns tokens or secret material.
#[tauri::command]
pub(crate) fn chatgpt_oauth_status() -> Result<ChatGptOAuthStatus, CommandError> {
    ChatGptOAuthStore::status().map_err(Into::into)
}

/// Start the interactive opt-in `ChatGPT` Plus/Pro OAuth 2.0 + PKCE (`S256`)
/// loopback flow (`S11-11a`). Stores tokens strictly in the OS secure keyring
/// and ensures the `openai` provider metadata row exists so `OpenAI` becomes
/// immediately selectable.
#[tauri::command]
pub(crate) async fn chatgpt_oauth_start(
    app: AppHandle,
) -> Result<ChatGptOAuthStatus, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let status = run_oauth_login_flow(
            |auth_url| {
                handle
                    .opener()
                    .open_url(auth_url, None::<&str>)
                    .map_err(|_| ChatGptOAuthError::BrowserOpenFailed)
            },
            DEFAULT_OAUTH_CALLBACK_TIMEOUT,
        )
        .map_err(CommandError::from)?;

        // Ensure the `openai` provider metadata row exists so OpenAI is
        // immediately usable without requiring a separate API key. If persisting
        // the provider row fails, roll back the newly saved OAuth keyring tokens.
        let db = handle.state::<Database>();
        let service = ProviderService::new(db.inner());
        ensure_openai_provider_row_or_rollback(&service, || {
            let _ = ChatGptOAuthStore::clear();
        })?;
        Ok(status)
    })
    .await;

    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("chatgpt_oauth_start blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "ChatGPT OAuth sign-in could not be completed",
            ))
        }
    }
}

/// Refresh stored opt-in `ChatGPT` Plus/Pro OAuth tokens in the OS keyring.
#[tauri::command]
pub(crate) async fn chatgpt_oauth_refresh() -> Result<ChatGptOAuthStatus, CommandError> {
    let outcome =
        tauri::async_runtime::spawn_blocking(move || refresh_tokens().map_err(CommandError::from))
            .await;

    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("chatgpt_oauth_refresh blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "ChatGPT OAuth token refresh could not be completed",
            ))
        }
    }
}

/// Disconnect opt-in `ChatGPT` Plus/Pro OAuth by removing its OS keyring entry.
/// Leaves any separate `OpenAI` API key untouched.
#[tauri::command]
pub(crate) fn chatgpt_oauth_disconnect() -> Result<(), CommandError> {
    ChatGptOAuthStore::clear().map_err(Into::into)
}
