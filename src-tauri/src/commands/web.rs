//! Tauri commands for opt-in agent Web Access configuration and keyed search
//! backend credentials (Stage 12 PR 6).
//!
//! Security invariants:
//! - Search API keys (`tavily`, `brave`, `exa`) live strictly in the OS secure
//!   keyring via [`CredentialStore`] under distinct `web_search_*` provider
//!   names; IPC exposes presence-only booleans and NEVER returns secret values.
//! - Errors are secret-free fixed-vocabulary [`CommandError`] values.

#![allow(clippy::needless_pass_by_value)]

use serde::Serialize;
use tauri::State;

use crate::application::settings::resolve_web_access_config;
use crate::application::web::search::{
    keyring_name_for_keyed_backend, WEB_SEARCH_KEYRING_BRAVE, WEB_SEARCH_KEYRING_EXA,
    WEB_SEARCH_KEYRING_TAVILY,
};
use crate::infrastructure::database::Database;
use crate::infrastructure::providers::credentials::CredentialStore;

use super::error::{CommandError, ErrorKind};

/// Secret-free snapshot of effective Web Access configuration and keyring
/// credential presence for Settings UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // Presence-only IPC DTO mirroring Settings toggles and 3 keyring flags.
pub(crate) struct WebAccessConfigView {
    pub enabled: bool,
    pub search_backend: String,
    pub searxng_url: Option<String>,
    pub max_results: usize,
    pub max_chars: usize,
    pub has_tavily_key: bool,
    pub has_brave_key: bool,
    pub has_exa_key: bool,
}

fn resolve_keyed_backend(backend: &str) -> Result<&'static str, CommandError> {
    keyring_name_for_keyed_backend(backend).ok_or_else(|| {
        CommandError::new(
            ErrorKind::InvalidInput,
            "search backend must be 'tavily', 'brave', or 'exa'",
        )
    })
}

/// Read effective web access settings and OS keyring API-key presence
/// (presence-only, never secret values).
#[tauri::command]
pub(crate) fn get_web_access_config(db: State<'_, Database>) -> WebAccessConfigView {
    let cfg = resolve_web_access_config(db.inner(), None);
    let has_tavily_key = CredentialStore::exists(WEB_SEARCH_KEYRING_TAVILY).unwrap_or(false);
    let has_brave_key = CredentialStore::exists(WEB_SEARCH_KEYRING_BRAVE).unwrap_or(false);
    let has_exa_key = CredentialStore::exists(WEB_SEARCH_KEYRING_EXA).unwrap_or(false);
    WebAccessConfigView {
        enabled: cfg.enabled,
        search_backend: cfg.search_backend.as_str().to_string(),
        searxng_url: cfg.searxng_url,
        max_results: cfg.max_results,
        max_chars: cfg.max_chars,
        has_tavily_key,
        has_brave_key,
        has_exa_key,
    }
}

/// Store or update a web search backend API key (`tavily`, `brave`, or `exa`)
/// strictly in the OS secure keyring.
#[tauri::command]
pub(crate) fn set_web_search_credential(
    backend: String,
    api_key: String,
) -> Result<(), CommandError> {
    let keyring_name = resolve_keyed_backend(&backend)?;
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "search API key must not be empty",
        ));
    }
    CredentialStore::add(keyring_name, trimmed).map_err(Into::into)
}

/// Remove a stored web search backend API key from the OS secure keyring.
#[tauri::command]
pub(crate) fn remove_web_search_credential(backend: String) -> Result<(), CommandError> {
    let keyring_name = resolve_keyed_backend(&backend)?;
    CredentialStore::remove(keyring_name).map_err(Into::into)
}

/// Report whether a web search backend API key is stored in the OS keyring
/// (presence-only, never the secret value).
#[tauri::command]
pub(crate) fn has_web_search_credential(backend: String) -> Result<bool, CommandError> {
    let keyring_name = resolve_keyed_backend(&backend)?;
    CredentialStore::exists(keyring_name).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_backend_validation_rejects_unknown_and_keyless_backends() {
        assert_eq!(
            resolve_keyed_backend("tavily").expect("tavily"),
            WEB_SEARCH_KEYRING_TAVILY
        );
        assert_eq!(
            resolve_keyed_backend("brave").expect("brave"),
            WEB_SEARCH_KEYRING_BRAVE
        );
        assert_eq!(
            resolve_keyed_backend("exa").expect("exa"),
            WEB_SEARCH_KEYRING_EXA
        );
        for invalid in ["ddg", "searxng", "openai", "", "tavily_secret_xyz"] {
            let err = resolve_keyed_backend(invalid).expect_err("must reject");
            assert_eq!(err.kind, ErrorKind::InvalidInput);
            assert!(!err.message.contains("tavily_secret_xyz"));
        }
    }

    #[test]
    fn empty_api_key_is_rejected_without_touching_keyring() {
        let err = set_web_search_credential("tavily".to_string(), "   ".to_string())
            .expect_err("empty key must fail");
        assert_eq!(err.kind, ErrorKind::InvalidInput);
    }
}
