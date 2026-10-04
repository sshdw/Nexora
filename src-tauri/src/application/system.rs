//! Crash / diagnostics / self-heal surface: a secret-free diagnostics bundle
//! plus a pre-update `SQLite` snapshot.
//!
//! What lives here (and what deliberately does not):
//!
//! - [`collect_bundle`] aggregates version, platform, storage, and crash
//!   facts the backend already records — no new collection, no writes. The
//!   bundle is secret-free by construction: it carries counts, versions, and
//!   fixed-vocabulary labels only — never message/prompt content,
//!   credentials, SQL, or file paths (ARCHITECTURE.md §9, §11;
//!   DATABASE.md §14). Crash evidence comes from the startup orphaned-run
//!   sweep (`crate::run` sweeps `status = 'running'` rows to `'error'` with
//!   the fixed [`SWEEP_MESSAGE`]): only rows carrying that exact fixed text
//!   surface individually (id, model, timestamp — model names are never
//!   credentials, DATABASE.md §7.8); every other error row contributes to the
//!   `error_runs` counter only, so arbitrary error text (which may quote user
//!   content) never crosses IPC.
//! - [`snapshot_to`] writes a consistent pre-update copy of the database with
//!   `SQLite`'s `VACUUM INTO` (atomic server-side copy — no file-locking
//!   hazards, no partial reads). It creates files only under the caller-given
//!   backup directory and returns the file *name* (never the full path: the
//!   app-data path may carry a user name).
//! - There is no in-memory "last errors ring": logging is a stderr-only sink
//!   (`infrastructure::logging`) with no retained records, and adding a
//!   global ring every error path must feed is out of scope. The swept-run
//!   rows above are the retained crash record.
//! - Self-heal beyond the snapshot is documented, not coded: the schema
//!   downgrade guard refuses to start on a newer database
//!   (`DatabaseError::SchemaTooNew`, `infrastructure/database.rs`), the FTS5
//!   indexes stay in sync through triggers (no manual rebuild affordance
//!   needed), and destructive recovery stays behind the existing
//!   user-confirmed data-management commands.
//!
//! Update *checking* (read-only, GitHub releases) lives in
//! [`crate::application::github::check_update`], reusing that module's HTTP
//! story — this module owns only the bundle and the snapshot.

use std::path::Path;

use serde::Serialize;

use crate::infrastructure::database::{Database, DatabaseError, MIGRATIONS};

/// Fixed sweep text the startup orphaned-run sweep writes
/// (`crate::run`): only error rows carrying exactly this text surface
/// individually in the bundle — everything else counts anonymously.
pub(crate) const SWEEP_MESSAGE: &str = "run interrupted by application shutdown";

/// How many swept crash rows the bundle carries (newest first).
const MAX_CRASHED_RUNS: i64 = 5;

/// Classified, secret-free failures of the diagnostics/snapshot path.
/// Messages are fixed vocabulary: no path, no SQL, no content.
#[derive(Debug)]
pub(crate) enum SystemError {
    /// A `SQLite` operation failed.
    Database(DatabaseError),
    /// The backup directory or snapshot file could not be written.
    Io,
}

impl std::fmt::Display for SystemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(err) => write!(f, "diagnostics database failure: {err}"),
            Self::Io => write!(f, "the snapshot could not be written"),
        }
    }
}

impl std::error::Error for SystemError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            Self::Io => None,
        }
    }
}

impl From<DatabaseError> for SystemError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

impl From<rusqlite::Error> for SystemError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Database(DatabaseError::Sqlite(err))
    }
}

/// Storage counters in the diagnostics bundle: row counts only, never
/// content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BundleCounts {
    pub conversations: i64,
    pub messages: i64,
    pub prompts: i64,
    pub agent_runs: i64,
    pub agent_steps: i64,
    pub agent_tasks: i64,
}

/// One retained crash row: the startup sweep's fixed-vocabulary rows only
/// (id, model, timestamp — no error text crosses IPC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CrashedRun {
    pub run_id: i64,
    pub model: String,
    pub started_at: i64,
}

/// Secret-free diagnostics bundle: versions, platform, storage facts, and
/// crash evidence. No tokens, no keys, no content, no paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DiagnosticsBundle {
    pub app_version: String,
    pub platform_os: String,
    pub platform_arch: String,
    pub schema_version: i64,
    pub schema_target: i64,
    /// On-disk database size in bytes (`None` when the file could not be
    /// statted — rendered as "unknown", never zero).
    pub db_size_bytes: Option<u64>,
    pub counts: BundleCounts,
    /// Whether the FTS5 search index is present (`messages_fts`).
    pub fts_present: bool,
    /// Newest swept crash rows (fixed sweep text only, at most
    /// [`MAX_CRASHED_RUNS`]).
    pub crashed_runs: Vec<CrashedRun>,
    /// Every `status = 'error'` run, including non-sweep failures whose
    /// text never leaves the database.
    pub error_runs: i64,
}

/// A pre-update snapshot copy: file name (never the full path) plus size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SnapshotInfo {
    pub file_name: String,
    pub size_bytes: u64,
    pub created_at: i64,
}

/// Highest migration version known to this build.
fn schema_target() -> i64 {
    MIGRATIONS
        .iter()
        .map(|(version, _)| *version)
        .max()
        .unwrap_or(0)
}

fn count(conn: &rusqlite::Connection, table: &str) -> Result<i64, SystemError> {
    // `table` is always one of the fixed literals below — never caller
    // input — so interpolation cannot inject.
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

/// Collect the secret-free diagnostics bundle.
///
/// `db_size_bytes` arrives from the IPC layer (which stats the database
/// file); every other fact is read from the shared connection.
///
/// # Errors
///
/// Returns [`SystemError::Database`] when a count or version query fails.
pub(crate) fn collect_bundle(
    db: &Database,
    db_size_bytes: Option<u64>,
) -> Result<DiagnosticsBundle, SystemError> {
    let conn = db.lock().map_err(SystemError::from)?;
    let schema_version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )?;
    let counts = BundleCounts {
        conversations: count(&conn, "conversations")?,
        messages: count(&conn, "messages")?,
        prompts: count(&conn, "prompts")?,
        agent_runs: count(&conn, "agent_runs")?,
        agent_steps: count(&conn, "agent_steps")?,
        agent_tasks: count(&conn, "agent_tasks")?,
    };
    let fts_present: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'messages_fts' AND type = 'table'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)?;
    let error_runs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_runs WHERE status = 'error'",
        [],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT id, model, started_at FROM agent_runs \
         WHERE status = 'error' AND error = ?1 \
         ORDER BY id DESC LIMIT ?2",
    )?;
    let crashed_runs = stmt
        .query_map(rusqlite::params![SWEEP_MESSAGE, MAX_CRASHED_RUNS], |row| {
            Ok(CrashedRun {
                run_id: row.get(0)?,
                model: row.get(1)?,
                started_at: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<CrashedRun>, _>>()?;
    Ok(DiagnosticsBundle {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        platform_os: std::env::consts::OS.to_string(),
        platform_arch: std::env::consts::ARCH.to_string(),
        schema_version,
        schema_target: schema_target(),
        db_size_bytes,
        counts,
        fts_present,
        crashed_runs,
        error_runs,
    })
}

/// Write a consistent pre-update snapshot of the database into
/// `backup_dir` via `SQLite`'s `VACUUM INTO` and report it.
///
/// The file name carries the Unix timestamp (`nexora-backup-<secs>.db`);
/// only the name (never the full path) is returned. The copy is atomic
/// server-side — a crash mid-copy leaves the live database untouched.
///
/// # Errors
///
/// Returns [`SystemError::Io`] when the directory or the snapshot file
/// cannot be written, [`SystemError::Database`] when the copy fails.
pub(crate) fn snapshot_to(db: &Database, backup_dir: &Path) -> Result<SnapshotInfo, SystemError> {
    std::fs::create_dir_all(backup_dir).map_err(|_| SystemError::Io)?;
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let file_name = format!("nexora-backup-{created_at}.db");
    let target = backup_dir.join(&file_name);
    let escaped = target.to_string_lossy().replace('\'', "''");
    let conn = db.lock().map_err(SystemError::from)?;
    conn.execute_batch(&format!("VACUUM INTO '{escaped}';"))
        .map_err(SystemError::from)?;
    drop(conn);
    let size_bytes = std::fs::metadata(&target)
        .map_err(|_| SystemError::Io)?
        .len();
    Ok(SnapshotInfo {
        file_name,
        size_bytes,
        created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    const SECRET_SENTINELS: [&str; 6] = [
        "sk-secret",
        "credential",
        "api_key",
        "api-key",
        "ghp_",
        "bearer ",
    ];

    fn bundle_json(bundle: &DiagnosticsBundle) -> String {
        serde_json::to_string(bundle).expect("bundle serializes")
    }

    fn assert_secret_free(json: &str) {
        let lowered = json.to_lowercase();
        for sentinel in SECRET_SENTINELS {
            assert!(
                !lowered.contains(sentinel),
                "diagnostics bundle must stay secret-free, found {sentinel:?} in {json}"
            );
        }
    }

    #[test]
    fn bundle_reports_real_app_data_and_stays_secret_free() {
        let db = in_memory_database();
        {
            let conn = db.lock().expect("lock connection");
            conn.execute("INSERT INTO conversations (title) VALUES ('diary')", [])
                .expect("insert conversation");
            let conv_id = conn.last_insert_rowid();
            // Planted secret-looking content: counts must count it, the
            // bundle must never echo it.
            conn.execute(
                "INSERT INTO messages (conversation_id, role, content) \
                 VALUES (?1, 'user', 'my key is sk-secret-PLANTED-12345')",
                rusqlite::params![conv_id],
            )
            .expect("insert message");
            conn.execute(
                "INSERT INTO prompts (title, content) VALUES ('t', 'credential=PLANTED')",
                [],
            )
            .expect("insert prompt");
        }
        let bundle = collect_bundle(&db, Some(12_345)).expect("collect bundle");

        assert_eq!(bundle.app_version, env!("CARGO_PKG_VERSION"));
        assert!(!bundle.platform_os.is_empty());
        assert!(!bundle.platform_arch.is_empty());
        assert_eq!(bundle.schema_version, schema_target());
        assert_eq!(bundle.schema_target, schema_target());
        assert_eq!(bundle.db_size_bytes, Some(12_345));
        assert_eq!(bundle.counts.conversations, 1);
        assert_eq!(bundle.counts.messages, 1);
        assert_eq!(bundle.counts.prompts, 1);
        assert!(bundle.fts_present, "fresh database carries the FTS index");

        let json = bundle_json(&bundle);
        assert_secret_free(&json);
        assert!(
            !json.contains("PLANTED"),
            "stored content must never echo into the bundle: {json}"
        );
        assert!(
            !json.contains("diary"),
            "conversation titles must never echo into the bundle: {json}"
        );
    }

    #[test]
    fn bundle_surfaces_only_sweep_crash_rows() {
        let db = in_memory_database();
        {
            let conn = db.lock().expect("lock connection");
            for (model, error) in [
                ("m-swept", SWEEP_MESSAGE),
                ("m-other", "boom sk-secret-PLANTED-999"),
            ] {
                conn.execute(
                    "INSERT INTO agent_runs (model, mode, status, error) \
                     VALUES (?1, 'supervised', 'error', ?2)",
                    rusqlite::params![model, error],
                )
                .expect("insert error run");
            }
        }
        let bundle = collect_bundle(&db, None).expect("collect bundle");

        assert_eq!(bundle.error_runs, 2, "every error run counts");
        assert_eq!(
            bundle.crashed_runs.len(),
            1,
            "only the fixed sweep text surfaces individually"
        );
        assert_eq!(bundle.crashed_runs[0].model, "m-swept");

        let json = bundle_json(&bundle);
        assert_secret_free(&json);
        assert!(
            !json.contains("boom") && !json.contains("PLANTED"),
            "arbitrary error text must never cross IPC: {json}"
        );
    }

    #[test]
    fn snapshot_writes_a_sized_file_and_names_no_path() {
        let db = in_memory_database();
        let dir = std::env::temp_dir().join(format!(
            "nexora-snap-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        let info = snapshot_to(&db, &dir).expect("snapshot writes");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            info.file_name.starts_with("nexora-backup-")
                && std::path::Path::new(&info.file_name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("db")),
            "unexpected snapshot name {}",
            info.file_name
        );
        assert!(
            !info.file_name.contains('/') && !info.file_name.contains('\\'),
            "only the file name crosses IPC, never a path: {}",
            info.file_name
        );
        assert!(info.size_bytes > 0, "the snapshot copy is non-empty");
        assert!(info.created_at > 0);
        let json = serde_json::to_string(&info).expect("snapshot serializes");
        assert_secret_free(&json);
    }
}
