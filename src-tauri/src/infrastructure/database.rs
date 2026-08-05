//! SQLite foundation: connection bootstrap and forward-only migration runner.
//!
//! Implements the Phase 0 database responsibilities of ROADMAP.md (SQLite
//! initialization and the migration runner) against the rules in DATABASE.md:
//!
//! - WAL journal mode and foreign-key enforcement are enabled on every
//!   connection (DATABASE.md §3).
//! - Schema changes are tracked in the `schema_version` bookkeeping table
//!   (DATABASE.md §4). The table is created by the runner itself and is not a
//!   numbered migration.
//! - Migrations are forward-only, incremental, and atomic. The application
//!   refuses to start when the database schema version is newer than the
//!   application's known migration set (DATABASE.md §5).
//!
//! Business-table migrations belong to Phase 1 (Database & Persistence) and are
//! intentionally absent here.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

/// Errors raised while opening or migrating the database.
#[derive(Debug)]
pub(crate) enum DatabaseError {
    /// A SQLite operation failed.
    Sqlite(rusqlite::Error),
    /// The on-disk schema version is newer than the application's migrations.
    SchemaTooNew {
        /// Schema version recorded in the database file.
        db_version: i64,
        /// Highest migration version known to this build of the application.
        app_version: i64,
    },
    /// The shared connection lock was poisoned by a panicking holder.
    Lock(String),
}

impl std::fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(err) => write!(f, "sqlite error: {err}"),
            Self::SchemaTooNew {
                db_version,
                app_version,
            } => write!(
                f,
                "database schema version {db_version} is newer than the \
                 application's known migrations ({app_version})"
            ),
            Self::Lock(msg) => write!(f, "database connection lock poisoned: {msg}"),
        }
    }
}

impl std::error::Error for DatabaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(err) => Some(err),
            Self::SchemaTooNew { .. } => None,
            Self::Lock(_) => None,
        }
    }
}

impl From<rusqlite::Error> for DatabaseError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

/// Forward-only, ordered database migrations.
///
/// Each entry is `(version, sql)` with monotonically increasing, unique
/// versions. Business-table migrations are appended here in Phase 1
/// (ROADMAP.md). The `schema_version` bookkeeping table is created by the
/// migration runner itself and is never a numbered migration.
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[];

/// Open the SQLite database at `path`, apply connection pragmas, and run any
/// pending migrations.
///
/// WAL mode, foreign-key enforcement, and a busy timeout are configured on the
/// returned connection (DATABASE.md §3).
pub(crate) fn open(path: impl AsRef<Path>) -> Result<Connection, DatabaseError> {
    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Apply the connection-level pragmas required by DATABASE.md §3.
fn configure(conn: &Connection) -> Result<(), DatabaseError> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA busy_timeout=5000;",
    )?;
    Ok(())
}

/// Create the `schema_version` bookkeeping table and apply pending migrations.
fn migrate(conn: &mut Connection) -> Result<(), DatabaseError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (\
             version INTEGER PRIMARY KEY,\
             applied_at INTEGER NOT NULL\
         );",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get::<_, i64>(0),
    )?;

    let target = target_version();
    if current > target {
        return Err(DatabaseError::SchemaTooNew {
            db_version: current,
            app_version: target,
        });
    }

    for &(version, sql) in MIGRATIONS {
        if version <= current {
            continue;
        }
        let applied_at = now_millis();
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
            params![version, applied_at],
        )?;
        tx.commit()?;
        log::info!("applied database migration v{version}");
    }

    Ok(())
}

/// Highest migration version known to this build of the application.
fn target_version() -> i64 {
    MIGRATIONS
        .iter()
        .map(|(version, _)| *version)
        .max()
        .unwrap_or(0)
}

/// Current time as Unix milliseconds.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(0))
        .unwrap_or(0)
}

/// Shared application-wide SQLite connection.
///
/// Opened exactly once during startup via [`open`] and registered as Tauri
/// managed state, so the single connection outlives `setup` and remains
/// available for the application's entire lifetime (ROADMAP.md Phase 0). Later
/// application components obtain it through `app.state::<Database>()`.
///
/// `rusqlite::Connection` is `Send` but not `Sync`; wrapping it in a `Mutex`
/// satisfies Tauri's `Send + Sync` requirement for managed state.
pub(crate) struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Wrap an already-opened, migrated connection in shared application state.
    pub(crate) const fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }

    /// Acquire the shared connection.
    ///
    /// Returns a guard providing exclusive access to the single connection.
    /// Holders must never write secrets (API keys, tokens, prompts, user
    /// messages) through the connection (ARCHITECTURE.md §12; DATABASE.md §14).
    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, DatabaseError> {
        self.conn
            .lock()
            .map_err(|err| DatabaseError::Lock(err.to_string()))
    }
}
