//! Telemetry controls + local usage ledger (privacy center).
//!
//! What leaves the machine, in one place — and the proof that nothing else
//! does:
//!
//! - [`status`] returns the egress-surface inventory (a static list; each
//!   entry carries its live on/off state where a switch exists) together
//!   with the aggregated ledger counters.
//! - The ledger itself is counts-only storage in the `usage_ledger` table
//!   (migration v9): the fixed `kind` vocabulary plus integer `day` /
//!   `count` columns. There is deliberately NO text content column —
//!   titles, prompts, message bodies, URLs, and error text can never be
//!   recorded here, enforced by the schema itself rather than by caller
//!   discipline. Rows older than [`RETENTION_DAYS`] days are pruned by the
//!   recording path.
//! - [`export_ledger`] returns the same aggregates as JSON-able data (the
//!   panel copies/downloads it exactly like the diagnostics bundle).
//! - [`wipe`] deletes every ledger row behind an explicit `confirmed` flag
//!   (the `refactor_apply` precedent: unconfirmed calls refuse instead of
//!   deleting).
//!
//! There is NO network upload of telemetry anywhere: no POST of stats
//! exists on this path (or anywhere else in the backend — the only POSTs
//! are the user's own provider chat calls and none carry ledger data).
//! Recording hooks are one-line best-effort calls at existing egress /
//! usage points (`send_message`, `start_run`, the four GitHub read
//! commands, `update_check`): a ledger failure is logged and never fails
//! the host operation.
//!
//! All failures are classified, secret-free [`PrivacyError`] values: fixed
//! vocabulary only — no kind strings, no SQL, no content.

use serde::Serialize;

use crate::application::github::GITHUB_KEYRING_ENTRY;
use crate::application::providers::ProviderService;
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::providers::credentials::CredentialStore;
use crate::infrastructure::providers::{anthropic, gemini, openai};

/// Ledger event kinds. Fixed vocabulary — the schema CHECK enforces exactly
/// this set, so no caller can record anything else.
pub(crate) const LEDGER_KINDS: [&str; 4] = ["agent_run", "message", "github_read", "update_check"];

/// How many days of ledger rows are kept; the recording path prunes older
/// rows on every write.
pub(crate) const RETENTION_DAYS: i64 = 90;

/// Seconds per ledger day-bucket.
const SECONDS_PER_DAY: i64 = 86_400;

/// Classified, secret-free failures of the privacy path. Messages are fixed
/// vocabulary: no kind text, no SQL, no content.
#[derive(Debug)]
pub(crate) enum PrivacyError {
    /// A `SQLite` operation failed.
    Database(DatabaseError),
    /// `wipe` was called without `confirmed = true`.
    Unconfirmed,
    /// A caller tried to record outside the fixed [`LEDGER_KINDS`]
    /// vocabulary (the schema CHECK would reject it anyway).
    UnknownKind,
}

impl std::fmt::Display for PrivacyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(err) => write!(f, "privacy database failure: {err}"),
            Self::Unconfirmed => write!(f, "the ledger wipe needs explicit confirmation"),
            Self::UnknownKind => write!(f, "unknown ledger event kind"),
        }
    }
}

impl std::error::Error for PrivacyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            Self::Unconfirmed | Self::UnknownKind => None,
        }
    }
}

impl From<DatabaseError> for PrivacyError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

impl From<rusqlite::Error> for PrivacyError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Database(DatabaseError::Sqlite(err))
    }
}

/// Live state of one egress surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SurfaceState {
    /// Sends when used (provider with configuration + stored key).
    On,
    /// Cannot send (missing configuration or key, or no such channel).
    Off,
    /// Sends only on the user's explicit action (never a timer).
    Manual,
    /// Never leaves the machine (local storage / clipboard only).
    Local,
}

/// One egress surface: where data can leave the machine, its live state,
/// and the honest detail (endpoint host or local-only note — never a key,
/// token, URL with secrets, or content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SurfaceEntry {
    pub id: String,
    pub title: String,
    pub destination: String,
    pub state: SurfaceState,
    pub detail: String,
}

/// One aggregated ledger row: event kind + day bucket + count. Counts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LedgerStat {
    pub kind: String,
    pub day: i64,
    pub count: i64,
}

/// Privacy status: the full surface inventory plus the ledger aggregates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PrivacyStatus {
    pub surfaces: Vec<SurfaceEntry>,
    pub stats: Vec<LedgerStat>,
    pub total_events: i64,
    pub retention_days: i64,
    pub oldest_day: Option<i64>,
}

/// Ledger export: the same aggregates plus metadata, shaped for copy /
/// download exactly like the diagnostics bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LedgerExport {
    pub exported_at: i64,
    pub retention_days: i64,
    pub stats: Vec<LedgerStat>,
}

/// Result of a confirmed ledger wipe: how many rows were deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WipeResult {
    pub deleted_rows: i64,
}

/// Current ledger day-bucket (Unix days). Returns `0` when the system
/// clock is unavailable; callers treat that as "unknown", never as epoch.
fn today_day() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs().cast_signed())
        .div_euclid(SECONDS_PER_DAY)
}

/// Record one ledger event for today and prune rows older than the
/// retention window. Unknown kinds are refused (the schema CHECK would
/// reject them anyway).
fn record_inner(db: &Database, kind: &str) -> Result<(), PrivacyError> {
    if !LEDGER_KINDS.contains(&kind) {
        return Err(PrivacyError::UnknownKind);
    }
    let conn = db.lock()?;
    let day = today_day();
    conn.execute(
        "INSERT INTO usage_ledger (kind, day, count) VALUES (?1, ?2, 1) \
         ON CONFLICT (kind, day) DO UPDATE SET count = count + 1",
        rusqlite::params![kind, day],
    )?;
    conn.execute(
        "DELETE FROM usage_ledger WHERE day < ?1",
        rusqlite::params![day - RETENTION_DAYS + 1],
    )?;
    Ok(())
}

/// Best-effort ledger recording for existing egress / usage points: a
/// ledger failure is logged and never fails the host operation.
pub(crate) fn record(db: &Database, kind: &str) {
    if let Err(err) = record_inner(db, kind) {
        log::warn!("usage ledger record failed: {err}");
    }
}

/// Destination host for a supported provider name (static map over the
/// hardcoded endpoints; the user-configured endpoint has no fixed host).
fn endpoint_destination(name: &str) -> &'static str {
    if name == openai::PROVIDER_NAME {
        "api.openai.com"
    } else if name == anthropic::PROVIDER_NAME {
        "api.anthropic.com"
    } else if name == gemini::PROVIDER_NAME {
        "generativelanguage.googleapis.com"
    } else if name == openai::XKIRO_NAME {
        "api.xkiro.com"
    } else if name == openai::OPENROUTER_NAME {
        "openrouter.ai"
    } else if name == openai::NVIDIA_NAME {
        "integrate.api.nvidia.com"
    } else if name == openai::OPENCODE_ZEN_NAME {
        "opencode.ai"
    } else {
        "user-configured base URL"
    }
}

/// Read the aggregated ledger rows (newest day first).
fn ledger_stats(conn: &rusqlite::Connection) -> Result<Vec<LedgerStat>, PrivacyError> {
    let mut stmt =
        conn.prepare("SELECT kind, day, count FROM usage_ledger ORDER BY day DESC, kind ASC")?;
    let stats = stmt
        .query_map([], |row| {
            Ok(LedgerStat {
                kind: row.get(0)?,
                day: row.get(1)?,
                count: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<LedgerStat>, _>>()?;
    Ok(stats)
}

/// Build the egress-surface inventory: the static list of everything that
/// can leave the machine, each with its live on/off state where a switch
/// exists. All provider/network probes here are local-only (configuration
/// + keyring presence) — inventorying never sends anything.
fn surface_inventory(db: &Database) -> Vec<SurfaceEntry> {
    let mut surfaces = Vec::new();
    let providers = ProviderService::new(db);
    for def in crate::infrastructure::providers::supported_providers() {
        let (state, detail) = match providers.health(&def.name) {
            Ok(health) => {
                if health.has_configuration && health.has_credential {
                    (
                        SurfaceState::On,
                        "configured with a stored key — sends prompts on runs and messages",
                    )
                } else if !health.has_configuration {
                    (SurfaceState::Off, "not configured — sends nothing")
                } else {
                    (SurfaceState::Off, "no key stored — sends nothing")
                }
            }
            Err(_) => (
                SurfaceState::Off,
                "status unavailable — treated as sending nothing",
            ),
        };
        surfaces.push(SurfaceEntry {
            id: format!("provider:{}", def.name),
            title: format!("{} API", def.display_name),
            destination: endpoint_destination(&def.name).to_string(),
            state,
            detail: detail.to_string(),
        });
    }
    let github_authenticated = CredentialStore::exists(GITHUB_KEYRING_ENTRY).unwrap_or(false);
    surfaces.push(SurfaceEntry {
        id: "github_api".to_string(),
        title: "GitHub issues / PRs / Actions".to_string(),
        destination: "api.github.com".to_string(),
        state: SurfaceState::Manual,
        detail: if github_authenticated {
            "token stored — authenticated reads on panel open only".to_string()
        } else {
            "no token — unauthenticated reads on panel open only".to_string()
        },
    });
    surfaces.push(SurfaceEntry {
        id: "release_check".to_string(),
        title: "Release check".to_string(),
        destination: "api.github.com (releases/latest)".to_string(),
        state: SurfaceState::Manual,
        detail: "button only — never automatic; the release page opens as text".to_string(),
    });
    surfaces.push(SurfaceEntry {
        id: "git_push".to_string(),
        title: "Git push".to_string(),
        destination: "workspace origin remote (user's own)".to_string(),
        state: SurfaceState::Manual,
        detail: "runs only on the explicit push action".to_string(),
    });
    surfaces.push(SurfaceEntry {
        id: "crash_upload".to_string(),
        title: "Crash upload".to_string(),
        destination: "none".to_string(),
        state: SurfaceState::Off,
        detail: "no crash upload exists — crash rows stay in the local database".to_string(),
    });
    surfaces.push(SurfaceEntry {
        id: "diagnostics".to_string(),
        title: "Diagnostics bundle".to_string(),
        destination: "none — clipboard or file chosen by the user".to_string(),
        state: SurfaceState::Local,
        detail: "copy-only; counts and versions, never content or keys".to_string(),
    });
    surfaces.push(SurfaceEntry {
        id: "usage_ledger".to_string(),
        title: "Usage ledger".to_string(),
        destination: "local SQLite (usage_ledger, 90 days)".to_string(),
        state: SurfaceState::Local,
        detail: "per-day counters only — export and wipe live here".to_string(),
    });
    surfaces
}

/// Collect the privacy status: surface inventory plus ledger aggregates.
///
/// # Errors
///
/// Returns [`PrivacyError::Database`] when the ledger cannot be read.
pub(crate) fn status(db: &Database) -> Result<PrivacyStatus, PrivacyError> {
    let conn = db.lock()?;
    let stats = ledger_stats(&conn)?;
    let total_events: i64 = stats.iter().map(|row| row.count).sum();
    let oldest_day = stats.iter().map(|row| row.day).min();
    drop(conn);
    Ok(PrivacyStatus {
        surfaces: surface_inventory(db),
        stats,
        total_events,
        retention_days: RETENTION_DAYS,
        oldest_day,
    })
}

/// Export the ledger aggregates for copy / download (same shape discipline
/// as the diagnostics bundle: metadata plus counts, never content).
///
/// # Errors
///
/// Returns [`PrivacyError::Database`] when the ledger cannot be read.
pub(crate) fn export_ledger(db: &Database) -> Result<LedgerExport, PrivacyError> {
    let conn = db.lock()?;
    let stats = ledger_stats(&conn)?;
    drop(conn);
    let exported_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs().cast_signed());
    Ok(LedgerExport {
        exported_at,
        retention_days: RETENTION_DAYS,
        stats,
    })
}

/// Delete every ledger row. Refuses without `confirmed = true` (the
/// `refactor_apply` precedent: the panel sends `confirmed: true` only from
/// its explicit confirm step).
///
/// # Errors
///
/// Returns [`PrivacyError::Unconfirmed`] without deleting anything when
/// `confirmed` is false, [`PrivacyError::Database`] on storage failure.
pub(crate) fn wipe(db: &Database, confirmed: bool) -> Result<WipeResult, PrivacyError> {
    if !confirmed {
        return Err(PrivacyError::Unconfirmed);
    }
    let conn = db.lock()?;
    let deleted_rows = conn.execute("DELETE FROM usage_ledger", [])?;
    Ok(WipeResult {
        deleted_rows: i64::try_from(deleted_rows).unwrap_or(i64::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    #[test]
    fn record_increments_today_bucket_per_kind() {
        let db = in_memory_database();
        record(&db, "agent_run");
        record(&db, "agent_run");
        record(&db, "message");
        let current = status(&db).expect("status reads the ledger");
        let runs = current
            .stats
            .iter()
            .find(|row| row.kind == "agent_run")
            .expect("agent_run row recorded");
        assert_eq!(runs.count, 2);
        assert_eq!(runs.day, today_day());
        let messages = current
            .stats
            .iter()
            .find(|row| row.kind == "message")
            .expect("message row recorded");
        assert_eq!(messages.count, 1);
        assert_eq!(current.total_events, 3);
        assert_eq!(current.retention_days, RETENTION_DAYS);
        assert_eq!(current.oldest_day, Some(today_day()));
    }

    #[test]
    fn record_refuses_unknown_kinds_without_writing() {
        let db = in_memory_database();
        let outcome = record_inner(&db, "prompt_text_planted");
        assert!(outcome.is_err(), "unknown kinds must be refused");
        let current = status(&db).expect("status reads the ledger");
        assert!(current.stats.is_empty(), "refused kinds leave no rows");
        assert_eq!(current.total_events, 0);
        assert_eq!(current.oldest_day, None);
    }

    #[test]
    fn record_prunes_rows_beyond_the_retention_window() {
        let db = in_memory_database();
        {
            let conn = db.lock().expect("lock the ledger");
            conn.execute(
                "INSERT INTO usage_ledger (kind, day, count) VALUES ('message', ?1, 4)",
                rusqlite::params![today_day() - RETENTION_DAYS - 1],
            )
            .expect("seed a stale row");
        }
        record(&db, "message");
        let current = status(&db).expect("status reads the ledger");
        assert!(
            current
                .stats
                .iter()
                .all(|row| row.day > today_day() - RETENTION_DAYS),
            "stale rows are pruned on write: {:?}",
            current.stats
        );
        assert_eq!(current.total_events, 1);
    }

    #[test]
    fn ledger_schema_holds_counts_only() {
        let db = in_memory_database();
        let conn = db.lock().expect("lock the ledger");
        let columns: Vec<(i64, String, String)> = conn
            .prepare("SELECT cid, name, type FROM pragma_table_info('usage_ledger')")
            .expect("inspect the ledger schema")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("read ledger columns")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect ledger columns");
        let names: Vec<&str> = columns.iter().map(|(_, name, _)| name.as_str()).collect();
        // The column set is the whole proof: exactly the fixed-vocabulary
        // `kind` plus integer `day` / `count` — no content column exists
        // for titles, prompts, bodies, URLs, or error text.
        assert_eq!(names, vec!["kind", "day", "count"]);
        let table_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'usage_ledger'",
                [],
                |row| row.get(0),
            )
            .expect("read the ledger definition");
        for kind in ["agent_run", "message", "github_read", "update_check"] {
            assert!(
                table_sql.contains(kind),
                "the schema CHECK must pin the {kind} vocabulary: {table_sql}"
            );
        }
    }

    #[test]
    fn wipe_needs_confirmation_and_clears_everything() {
        let db = in_memory_database();
        record(&db, "github_read");
        let refused = wipe(&db, false);
        assert!(
            matches!(refused, Err(PrivacyError::Unconfirmed)),
            "unconfirmed wipe must refuse, got {refused:?}"
        );
        let kept = status(&db).expect("status reads the ledger");
        assert_eq!(kept.total_events, 1, "unconfirmed wipe deletes nothing");
        let wiped = wipe(&db, true).expect("confirmed wipe clears the ledger");
        assert_eq!(wiped.deleted_rows, 1);
        let cleared = status(&db).expect("status reads the ledger");
        assert!(cleared.stats.is_empty());
        assert_eq!(cleared.total_events, 0);
    }

    #[test]
    fn export_carries_aggregates_and_no_content() {
        let db = in_memory_database();
        record(&db, "update_check");
        let exported = export_ledger(&db).expect("export reads the ledger");
        assert_eq!(exported.retention_days, RETENTION_DAYS);
        assert!(exported.exported_at > 0);
        assert_eq!(exported.stats.len(), 1);
        assert_eq!(exported.stats[0].kind, "update_check");
        assert_eq!(exported.stats[0].count, 1);
        let serialized = serde_json::to_string(&exported).expect("export serializes");
        assert!(!serialized.contains("prompt"), "export holds counts only");
    }

    #[test]
    fn inventory_lists_every_egress_surface_with_honest_states() {
        let db = in_memory_database();
        let current = status(&db).expect("status builds the inventory");
        let ids: Vec<&str> = current
            .surfaces
            .iter()
            .map(|surface| surface.id.as_str())
            .collect();
        for required in [
            "github_api",
            "release_check",
            "git_push",
            "crash_upload",
            "diagnostics",
            "usage_ledger",
        ] {
            assert!(
                ids.contains(&required),
                "inventory must list {required}: {ids:?}"
            );
        }
        assert!(
            ids.iter().any(|id| id.starts_with("provider:")),
            "inventory must list provider surfaces: {ids:?}"
        );
        let crash = current
            .surfaces
            .iter()
            .find(|surface| surface.id == "crash_upload")
            .expect("crash surface listed");
        assert_eq!(crash.state, SurfaceState::Off);
        let release = current
            .surfaces
            .iter()
            .find(|surface| surface.id == "release_check")
            .expect("release surface listed");
        assert_eq!(release.state, SurfaceState::Manual);
        for surface in &current.surfaces {
            assert!(
                !surface.destination.contains("sk-"),
                "destinations never carry secrets: {}",
                surface.destination
            );
        }
    }
}
