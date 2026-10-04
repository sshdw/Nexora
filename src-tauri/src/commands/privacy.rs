//! Privacy-center IPC commands: the Tauri side of the telemetry-controls
//! surface in the health panel.
//!
//! Thin translation only (ARCHITECTURE.md §5): the three commands delegate
//! to the application-layer privacy service
//! ([`crate::application::privacy`]) and map failures into secret-free
//! [`CommandError`] values. No business logic lives here beyond that
//! translation.
//!
//! Command-shape decision (one feature area, THREE commands — the allowed
//! maximum): `privacy_status` (surface inventory + ledger aggregates),
//! `privacy_export` (the same aggregates as copy/download JSON), and
//! `privacy_wipe` (user-confirmed delete behind the `confirmed` flag, the
//! `refactor_apply` precedent). Recording needs no command: the service's
//! best-effort [`crate::application::privacy::record`] is called from the
//! existing egress / usage points after success.
//!
//! There is no network upload of telemetry anywhere on this path — or
//! anywhere else in the backend. The only POSTs are the user's own
//! provider chat calls, and none carry ledger data.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::privacy::{LedgerExport, PrivacyStatus, WipeResult};
use crate::infrastructure::database::Database;

use super::error::CommandError;

/// Collect the privacy status: every egress surface with its live state
/// plus the local ledger aggregates. Read-only and local-only.
#[tauri::command]
pub(crate) fn privacy_status(db: State<'_, Database>) -> Result<PrivacyStatus, CommandError> {
    crate::application::privacy::status(db.inner()).map_err(CommandError::from)
}

/// Export the ledger aggregates as copy/download JSON (counts only —
/// the same shape discipline as the diagnostics bundle).
#[tauri::command]
pub(crate) fn privacy_export(db: State<'_, Database>) -> Result<LedgerExport, CommandError> {
    crate::application::privacy::export_ledger(db.inner()).map_err(CommandError::from)
}

/// Wipe every ledger row. Refuses without `confirmed = true` (the panel
/// sends confirmation only from its explicit confirm step).
#[tauri::command]
pub(crate) fn privacy_wipe(
    confirmed: bool,
    db: State<'_, Database>,
) -> Result<WipeResult, CommandError> {
    crate::application::privacy::wipe(db.inner(), confirmed).map_err(CommandError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::privacy::PrivacyError;
    use crate::commands::error::ErrorKind;
    use crate::infrastructure::database::DatabaseError;

    const SOURCE: &str = include_str!("privacy.rs");

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn privacy_error_mapping_is_classified_and_secret_free() {
        let mapped = CommandError::from(PrivacyError::Unconfirmed);
        assert_eq!(mapped.kind, ErrorKind::ConfirmationRequired);
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(PrivacyError::UnknownKind);
        assert_eq!(mapped.kind, ErrorKind::InvalidInput);
        assert_eq!(mapped.message, "unknown ledger event kind");
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(PrivacyError::Database(DatabaseError::Lock(
            "poisoned sk-secret-PLANTED".to_string(),
        )));
        assert_eq!(mapped.kind, ErrorKind::Database);
        assert!(safe_message(&mapped));
        assert!(
            !mapped.message.contains("PLANTED"),
            "lock detail must never leak: {}",
            mapped.message
        );
    }

    /// Static wiring check: the privacy commands stay thin translation —
    /// service delegation only. No process spawning, no network client of
    /// their own (there is no telemetry upload anywhere), no silent
    /// deletes outside the confirmed wipe. Needles are built with
    /// `concat!` so this test's own source never matches them.
    #[test]
    fn privacy_commands_stay_thin_translation() {
        for needle in ["privacy_status", "privacy_export", "privacy_wipe"] {
            assert!(
                SOURCE.contains(needle),
                "the privacy commands must exist, missing {needle:?}"
            );
        }
        for needle in [
            concat!("std::process", "::"),
            concat!("tokio", "::process"),
            concat!(".", "post("),
            concat!(".", "put("),
            concat!("reqwest", "::"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/privacy.rs must not spawn or speak HTTP, found {needle:?}"
            );
        }
    }
}
