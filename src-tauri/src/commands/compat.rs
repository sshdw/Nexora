//! Tauri commands over the existing [`CompatEndpointService`].
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates
//! to the existing application-layer compatible-endpoint service and converts
//! its classified errors into safe [`CommandError`] values. Only endpoint
//! metadata (base URL, model, organization, headers, presence booleans) ever
//! crosses this boundary — the API key lives exclusively in the OS secure
//! keyring and is reported only as presence (FR-014; ARCHITECTURE.md §12).

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::compat::{CompatEndpointService, CompatStatus};
use crate::infrastructure::database::Database;
use crate::infrastructure::providers::credentials::CredentialStore;
use crate::infrastructure::providers::openai::{CompatConfig, COMPAT_NAME};

use super::error::CommandError;

/// Read the stored OpenAI-compatible endpoint configuration.
///
/// Returns the endpoint metadata (base URL, model, organization, headers,
/// tool support). Carries no secret: the API key is keyring-only and never
/// returned here.
#[tauri::command]
pub(crate) fn get_compat_config(db: State<'_, Database>) -> Result<CompatConfig, CommandError> {
    CompatEndpointService::new(db.inner())
        .read_config()
        .map_err(Into::into)
}

/// Persist the OpenAI-compatible endpoint configuration.
///
/// The backend validates the config and rejects it unchanged on failure; the
/// error is a secret-free category. The API key is never part of the config —
/// store it via the credential commands (`add_provider_credential` /
/// `update_provider_credential` for `openai_compat`).
#[tauri::command]
pub(crate) fn set_compat_config(
    config: CompatConfig,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    CompatEndpointService::new(db.inner())
        .write_config(&config)
        .map_err(Into::into)
}

/// Report the UI-facing status of the OpenAI-compatible endpoint: field
/// presence booleans, URL validity, keyring credential presence, and overall
/// readiness. Carries metadata only — never a secret value.
#[tauri::command]
pub(crate) fn compat_status(db: State<'_, Database>) -> Result<CompatStatus, CommandError> {
    let has_credential = CredentialStore::exists(COMPAT_NAME).map_err(CommandError::from)?;
    CompatEndpointService::new(db.inner())
        .status(has_credential)
        .map_err(Into::into)
}
