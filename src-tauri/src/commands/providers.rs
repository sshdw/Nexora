//! Tauri commands over the existing [`ProviderService`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer provider-metadata service and converts its
//! classified errors into safe [`CommandError`] values. Provider credentials
//! are never returned here вЂ” only metadata and credential *presence*
//! (FR-014), never the secret value (ARCHITECTURE.md В§12; DATABASE.md В§14).

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager, State};

use crate::application::capabilities::{
    list_all_model_capabilities, refresh_connection_metadata, resolve_model_capabilities,
    set_manual_override, set_scoped_manual_override, ConnectionModelCapabilities,
    ModelCapabilityOverride,
};
use crate::application::providers::ProviderHealth;
use crate::application::providers::ProviderService;
use crate::application::settings::SettingScope;
use crate::infrastructure::database::Database;
use crate::infrastructure::providers::{is_retired_cli_provider, SupportedProvider};
use crate::infrastructure::repository::providers::Provider;

use super::error::{CommandError, ErrorKind};

/// Return every provider supported by this build, with its supported models
/// (DATABASE.md В§7.5: model lists are hardcoded in the MVP). This is the
/// read-only source of truth for provider/model selection; the UI never invents
/// providers or models. Carries metadata only вЂ” never a credential.
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
    ProviderService::new(db.inner())
        .available()
        .map_err(Into::into)
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

/// Probe the local health of one provider (WS-A.5).
///
/// Thin translation over [`ProviderService::health`]: local-only probe (no
/// network) returning the readiness verdict, presence booleans, and a
/// `last_checked` Unix-seconds timestamp. Carries metadata only — never a
/// credential value.
#[tauri::command]
pub(crate) fn provider_health(
    name: String,
    db: State<'_, Database>,
) -> Result<ProviderHealth, CommandError> {
    ProviderService::new(db.inner())
        .health(&name)
        .map_err(Into::into)
}

/// Register a new provider definition.
#[tauri::command]
pub(crate) fn create_provider(
    name: String,
    display_name: String,
    db: State<'_, Database>,
) -> Result<i64, CommandError> {
    if is_retired_cli_provider(&name)
        || name.trim() == crate::infrastructure::providers::chatgpt_plan::PROVIDER_NAME
    {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "this provider requires its dedicated setup flow",
        ));
    }
    ProviderService::new(db.inner())
        .create(&name, &display_name)
        .map_err(Into::into)
}

/// Remove a provider definition.
#[tauri::command]
pub(crate) fn remove_provider(id: i64, db: State<'_, Database>) -> Result<(), CommandError> {
    ProviderService::new(db.inner())
        .remove(id)
        .map_err(Into::into)
}

/// Resolve capabilities for `(connection_id, model_id)` using strict precedence:
/// `ManualOverride > ApiMetadata > BuiltInCatalog > Unknown`.
#[tauri::command]
pub(crate) fn get_model_capabilities(
    connection_id: String,
    model_id: String,
    db: State<'_, Database>,
) -> Result<ConnectionModelCapabilities, CommandError> {
    if connection_id.trim().is_empty() || model_id.trim().is_empty() {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "connection_id and model_id are required",
        ));
    }
    Ok(resolve_model_capabilities(
        db.inner(),
        connection_id.trim(),
        model_id.trim(),
    ))
}

/// List resolved capabilities across all supported connections & models.
#[tauri::command]
pub(crate) fn list_model_capabilities(db: State<'_, Database>) -> Vec<ConnectionModelCapabilities> {
    list_all_model_capabilities(db.inner())
}

/// Persist or clear a user manual capability override for `(connection_id, model_id)`.
#[tauri::command]
pub(crate) fn set_model_capability_override(
    connection_id: String,
    model_id: String,
    override_spec: Option<ModelCapabilityOverride>,
    scope: Option<String>,
    db: State<'_, Database>,
) -> Result<ConnectionModelCapabilities, CommandError> {
    if connection_id.trim().is_empty() || model_id.trim().is_empty() {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "connection_id and model_id are required",
        ));
    }
    if let Some(raw_scope) = scope {
        let parsed = SettingScope::parse(&raw_scope).ok_or_else(|| {
            CommandError::new(
                ErrorKind::InvalidInput,
                "setting scope must be 'personal' or 'project'",
            )
        })?;
        return set_scoped_manual_override(
            db.inner(),
            parsed,
            connection_id.trim(),
            model_id.trim(),
            override_spec,
        )
        .map_err(Into::into);
    }
    set_manual_override(
        db.inner(),
        connection_id.trim(),
        model_id.trim(),
        override_spec,
    )
    .map_err(Into::into)
}

/// Clear a manual capability override for `(connection_id, model_id)` from
/// Personal `app_settings` or active Project `.nexora/config.json`.
#[tauri::command]
pub(crate) fn clear_model_capability_override(
    connection_id: String,
    model_id: String,
    scope: Option<String>,
    db: State<'_, Database>,
) -> Result<ConnectionModelCapabilities, CommandError> {
    set_model_capability_override(connection_id, model_id, None, scope, db)
}

/// Refresh zero-token `/models` metadata from the provider API for `connection_id`.
#[tauri::command]
pub(crate) async fn refresh_provider_model_metadata(
    app: AppHandle,
    connection_id: String,
) -> Result<Vec<ConnectionModelCapabilities>, CommandError> {
    if connection_id.trim().is_empty() {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "connection_id is required",
        ));
    }
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        refresh_connection_metadata(
            db.inner(),
            connection_id.trim(),
            std::sync::Arc::new(crate::async_spawner::TauriSpawner),
        )
        .map_err(|err| CommandError::new(ErrorKind::Request, err.to_string()))
    })
    .await;
    match outcome {
        Ok(res) => res,
        Err(_) => Err(CommandError::new(
            ErrorKind::Request,
            "provider metadata refresh failed",
        )),
    }
}
