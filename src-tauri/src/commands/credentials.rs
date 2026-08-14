//! Tauri commands over the existing [`CredentialStore`]
//! (Phase 10.2 — Tauri Command Layer).
//!
//! Provider credentials live exclusively in the OS secure keyring
//! (ARCHITECTURE.md §12; DATABASE.md §14) and are never written to SQLite.
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

use crate::infrastructure::providers::credentials::CredentialStore;

use super::error::CommandError;

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