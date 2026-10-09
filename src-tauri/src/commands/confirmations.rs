//! Server-side confirmation minting (NEX-SEC-004).
//!
//! Thin translation only (ARCHITECTURE.md §5): `request_confirmation` mints
//! one single-use confirmation id in the application-layer
//! [`ConfirmationRegistry`](crate::application::confirmations::ConfirmationRegistry)
//! and returns it. The honest frontend calls this from the confirm click and
//! passes the minted id to the destructive command, which consumes it
//! atomically. No business logic lives here beyond that translation; the
//! scope allowlist, TTL, and single-use enforcement stay in the application
//! layer.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
// (Same justification as the other command modules, e.g. conversations.rs.)
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::confirmations::ManagedConfirmations;

use super::error::CommandError;

/// Mint one single-use confirmation id for `scope` (`"terminal"` or
/// `"data_management"`) and return it. `summary` is audit context only (a
/// command excerpt or operation name, length-capped server-side, never
/// echoed) — it grants nothing by itself. The destructive command consumes
/// the id exactly once within its TTL; forged, expired, reused, and
/// cross-scope ids are refused there with no execution.
///
/// # Errors
///
/// Classified [`CommandError`]s for an unknown scope (`InvalidInput`).
/// Secret-free by construction: neither the scope nor the summary is echoed.
#[tauri::command]
pub(crate) fn request_confirmation(
    scope: String,
    summary: String,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<String, CommandError> {
    confirmations
        .request(scope.as_str(), summary.as_str())
        .map_err(CommandError::from)
}
