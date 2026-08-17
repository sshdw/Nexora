//! Tauri commands over the existing [`ProviderService`]
//! (Phase 10.2 — Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer provider-metadata service and converts its
//! classified errors into safe [`CommandError`] values. Provider credentials
//! are never returned here — only metadata and credential *presence*
//! (FR-014), never the secret value (ARCHITECTURE.md §12; DATABASE.md §14).

use tauri::State;

use crate::application::providers::ProviderService;
use crate::infrastructure::database::Database;
use crate::infrastructure::providers::SupportedProvider;
use crate::infrastructure::repository::providers::Provider;

use super::error::CommandError;

/// Return every provider supported by this build, with its supported models
/// (DATABASE.md §7.5: model lists are hardcoded in the MVP). This is the
/// read-only source of truth for provider/model selection; the UI never invents
/// providers or models. Carries metadata only — never a credential.
#[tauri::command]
pub(crate) fn supported_providers() -> Vec<SupportedProvider> {
    crate::infrastructure::providers::supported_providers()
}

/// List all configured providers (metadata only, ordered by `id`).
#[tauri::command]
pub(crate) fn list_providers(db: State<'_, Database>) -> Result<Vec<Provider>, CommandError> {
    ProviderService::new(db.inner()).list().map_err(Into::into)
}

/// List the providers that are configured **and** have stored credentials.
#[tauri::command]
pub(crate) fn list_available_providers(
    db: State<'_, Database>,
) -> Result<Vec<Provider>, CommandError> {
    ProviderService::new(db.inner()).available().map_err(Into::into)
}

/// Report whether a provider is configured and has stored credentials.
#[tauri::command]
pub(crate) fn is_provider_available(
    name: String,
    db: State<'_, Database>,
) -> Result<bool, CommandError> {
    ProviderService::new(db.inner())
        .is_available(&name)
        .map_err(Into::into)
}

/// Register a new provider definition.
#[tauri::command]
pub(crate) fn create_provider(
    name: String,
    display_name: String,
    db: State<'_, Database>,
) -> Result<i64, CommandError> {
    ProviderService::new(db.inner())
        .create(&name, &display_name)
        .map_err(Into::into)
}

/// Remove a provider definition.
#[tauri::command]
pub(crate) fn remove_provider(id: i64, db: State<'_, Database>) -> Result<(), CommandError> {
    ProviderService::new(db.inner()).remove(id).map_err(Into::into)
}