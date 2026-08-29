//! `SQLite` foundation: connection bootstrap and forward-only migration runner.
//!
//! Implements the Phase 0 database responsibilities of ROADMAP.md (`SQLite`
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
//! Business-table migrations materialize the DATABASE.md schema (§7–§11) via
//! [`MIGRATIONS`]: the base tables and their functional indexes (v1), the FTS5
//! search indexes and their synchronization triggers (v2), the
//! `updated_at` maintenance triggers (v3), and the agent run persistence
//! tables `agent_runs` / `agent_steps` (v4, Task 4.2).

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

/// Errors raised while opening or migrating the database.
#[derive(Debug)]
pub(crate) enum DatabaseError {
    /// A `SQLite` operation failed.
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
            Self::SchemaTooNew { .. } | Self::Lock(_) => None,
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
/// versions (DATABASE.md §4–§5). The `schema_version` bookkeeping table is
/// created by the migration runner itself and is never a numbered migration.
///
/// The migration set materializes the schema documented in DATABASE.md:
///
/// - v1: the base tables — `providers` (§7.5), `conversations` (§7.1),
///   `messages` (§7.2), `prompts` (§7.3), `attachments` (§7.4), and
///   `app_settings` (§7.6) — with their documented columns, defaults, CHECK
///   constraints, and foreign keys, plus the functional indexes of §8.
/// - v2: the FTS5 search indexes `conversations_fts` / `messages_fts` /
///   `prompts_fts` (§10) and the synchronization triggers that keep them
///   current (§11). Update/delete synchronization uses
///   `DELETE FROM ..._fts WHERE rowid = old.id`; the FTS5 special `'delete'`
///   command is unsupported for these regular content-storing tables.
/// - v3: the `updated_at` maintenance triggers (§11) for `conversations`
///   (title/status) and `prompts` (title/content).
/// - v4: the agent run persistence tables `agent_runs` (§7.8) and
///   `agent_steps` (§7.9) with their columns, defaults, CHECK constraints,
///   foreign keys (§9), `UNIQUE(run_id, seq)`, and functional indexes (§8) —
///   agent roadmap, Task 4.2.
/// - v5: the spend-guard columns `spent_micro_usd` / `limit_micro_usd` and the
///   widened `status` CHECK (`spend_limit_exceeded`, Task 4.3) via a validated
///   rebuild of `agent_runs` (DATABASE.md §5).
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[
    // v1 — base tables and functional indexes (DATABASE.md §7, §8).
    (
        1,
        r"CREATE TABLE providers (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    name TEXT NOT NULL UNIQUE CHECK (length(name) > 0 AND length(name) <= 100),
    display_name TEXT NOT NULL CHECK (length(display_name) > 0)
);

CREATE TABLE conversations (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    title TEXT NOT NULL DEFAULT 'Untitled Conversation'
        CHECK (length(title) > 0 AND length(title) <= 500),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'archived')),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (created_at > 0),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        CHECK (updated_at >= created_at)
);

CREATE TABLE messages (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    conversation_id INTEGER NOT NULL
        CHECK (conversation_id > 0)
        REFERENCES conversations (id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content TEXT NOT NULL CHECK (length(content) > 0),
    provider_id INTEGER
        CHECK (provider_id IS NULL OR provider_id > 0)
        REFERENCES providers (id) ON DELETE SET NULL,
    model_name TEXT CHECK (length(model_name) <= 200),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (created_at > 0)
);

CREATE TABLE prompts (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    title TEXT NOT NULL CHECK (length(title) > 0 AND length(title) <= 200),
    content TEXT NOT NULL
        CHECK (length(content) > 0 AND length(content) <= 10000),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (created_at > 0),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        CHECK (updated_at >= created_at)
);

CREATE TABLE attachments (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    conversation_id INTEGER NOT NULL
        CHECK (conversation_id > 0)
        REFERENCES conversations (id) ON DELETE CASCADE,
    message_id INTEGER
        CHECK (message_id IS NULL OR message_id > 0)
        REFERENCES messages (id) ON DELETE CASCADE,
    file_name TEXT NOT NULL
        CHECK (length(file_name) > 0 AND length(file_name) <= 255),
    file_path TEXT NOT NULL CHECK (length(file_path) > 0),
    file_size_bytes INTEGER CHECK (file_size_bytes >= 0),
    mime_type TEXT CHECK (length(mime_type) <= 127)
);

CREATE TABLE app_settings (
    key TEXT PRIMARY KEY CHECK (length(key) > 0 AND length(key) <= 200),
    value TEXT CHECK (length(value) <= 10000)
);

CREATE INDEX idx_messages_conversation_created
    ON messages (conversation_id, created_at);

CREATE INDEX idx_attachments_conversation
    ON attachments (conversation_id);

CREATE INDEX idx_attachments_message
    ON attachments (message_id);

CREATE INDEX idx_conversations_status_updated
    ON conversations (status, updated_at);

CREATE INDEX idx_providers_name
        ON providers (name);
",
    ),
    // v2 — FTS5 search indexes and their synchronization triggers
    // (DATABASE.md §10–§11). Update/delete synchronization deletes index rows
    // by `rowid`; the FTS5 special `'delete'` command is unsupported for these
    // regular content-storing tables.
    (
        2,
        r"CREATE VIRTUAL TABLE conversations_fts USING fts5(title);

CREATE VIRTUAL TABLE messages_fts USING fts5(content);

CREATE VIRTUAL TABLE prompts_fts USING fts5(title, content);

CREATE TRIGGER conversations_fts_insert AFTER INSERT ON conversations BEGIN
    INSERT INTO conversations_fts (rowid, title) VALUES (new.id, new.title);
END;

CREATE TRIGGER conversations_fts_update AFTER UPDATE OF title ON conversations BEGIN
    DELETE FROM conversations_fts WHERE rowid = old.id;
    INSERT INTO conversations_fts (rowid, title) VALUES (new.id, new.title);
END;

CREATE TRIGGER conversations_fts_delete AFTER DELETE ON conversations BEGIN
    DELETE FROM conversations_fts WHERE rowid = old.id;
END;

CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER messages_fts_update AFTER UPDATE OF content ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
    INSERT INTO messages_fts (rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
END;

CREATE TRIGGER prompts_fts_insert AFTER INSERT ON prompts BEGIN
    INSERT INTO prompts_fts (rowid, title, content)
        VALUES (new.id, new.title, new.content);
END;

CREATE TRIGGER prompts_fts_update AFTER UPDATE OF title, content ON prompts BEGIN
    DELETE FROM prompts_fts WHERE rowid = old.id;
    INSERT INTO prompts_fts (rowid, title, content)
        VALUES (new.id, new.title, new.content);
END;

CREATE TRIGGER prompts_fts_delete AFTER DELETE ON prompts BEGIN
        DELETE FROM prompts_fts WHERE rowid = old.id;
END;
",
    ),
    // v3 — `updated_at` maintenance triggers (DATABASE.md §11). Messages and
    // attachments are excluded: messages are immutable and attachment linking
    // is not a semantic modification.
    (
        3,
        r"CREATE TRIGGER conversations_touch_updated_at
AFTER UPDATE OF title, status ON conversations
BEGIN
    UPDATE conversations
        SET updated_at = (unixepoch())
        WHERE id = old.id;
END;

CREATE TRIGGER prompts_touch_updated_at
AFTER UPDATE OF title, content ON prompts
BEGIN
    UPDATE prompts
        SET updated_at = (unixepoch())
        WHERE id = old.id;
END;
",
    ),
    // v4 — agent run persistence tables (DATABASE.md §7.8, §7.9; agent
    // roadmap, Task 4.2). One `agent_runs` row per opt-in-persisted agent
    // run; append-only `agent_steps` rows for each model turn, tool call,
    // and parked approval decision. `conversation_id` is NULL until the
    // Task 5.1 IPC layer wires runs to conversations; the column and its
    // CASCADE exist from this migration so the privacy doctrine holds from
    // day one (D50). Model names are never credentials (DATABASE.md §14).
    (
        4,
        r"CREATE TABLE agent_runs (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    conversation_id INTEGER
        CHECK (conversation_id IS NULL OR conversation_id > 0)
        REFERENCES conversations (id) ON DELETE CASCADE,
    model TEXT NOT NULL CHECK (length(model) > 0),
    mode TEXT NOT NULL
        CHECK (mode IN ('supervised', 'semi_autonomous', 'full_autonomous')),
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'completed', 'cancelled', 'budget_exhausted', 'error')),
    started_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (started_at > 0),
    finished_at INTEGER CHECK (finished_at IS NULL OR finished_at > 0),
    total_steps INTEGER NOT NULL DEFAULT 0 CHECK (total_steps >= 0),
    final_content TEXT,
    error TEXT
);

CREATE TABLE agent_steps (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    run_id INTEGER NOT NULL
        CHECK (run_id > 0)
        REFERENCES agent_runs (id) ON DELETE CASCADE,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    kind TEXT NOT NULL CHECK (kind IN ('model_turn', 'tool_call', 'approval')),
    tool_name TEXT CHECK (tool_name IS NULL OR length(tool_name) > 0),
    arguments TEXT,
    observation TEXT,
    status TEXT
        CHECK (status IS NULL OR status IN ('succeeded', 'failed', 'denied', 'cancelled')),
    started_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (started_at > 0),
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    UNIQUE (run_id, seq)
);

CREATE INDEX idx_agent_steps_run_seq
    ON agent_steps (run_id, seq);

CREATE INDEX idx_agent_runs_conversation
    ON agent_runs (conversation_id);

CREATE INDEX idx_agent_runs_started
    ON agent_runs (started_at);
",
    ),
    // v5 — spend-guard columns and widened status CHECK (DATABASE.md §7.8,
    // §5; Task 4.3). SQLite cannot ALTER a CHECK, so the table is rebuilt
    // (create new → copy → drop → rename) with foreign_keys OFF pre-tx.
    (
        5,
        r"CREATE TABLE agent_runs_new (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    conversation_id INTEGER
        CHECK (conversation_id IS NULL OR conversation_id > 0)
        REFERENCES conversations (id) ON DELETE CASCADE,
    model TEXT NOT NULL CHECK (length(model) > 0),
    mode TEXT NOT NULL
        CHECK (mode IN ('supervised', 'semi_autonomous', 'full_autonomous')),
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'completed', 'cancelled', 'budget_exhausted', 'spend_limit_exceeded', 'error')),
    started_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (started_at > 0),
    finished_at INTEGER CHECK (finished_at IS NULL OR finished_at > 0),
    total_steps INTEGER NOT NULL DEFAULT 0 CHECK (total_steps >= 0),
    final_content TEXT,
    error TEXT,
    spent_micro_usd INTEGER CHECK (spent_micro_usd IS NULL OR spent_micro_usd >= 0),
    limit_micro_usd INTEGER CHECK (limit_micro_usd IS NULL OR limit_micro_usd >= 0)
);

INSERT INTO agent_runs_new
    (id, conversation_id, model, mode, status, started_at, finished_at,
     total_steps, final_content, error)
    SELECT id, conversation_id, model, mode, status, started_at, finished_at,
           total_steps, final_content, error
    FROM agent_runs;

DROP TABLE agent_runs;
ALTER TABLE agent_runs_new RENAME TO agent_runs;

CREATE INDEX idx_agent_runs_conversation ON agent_runs (conversation_id);
CREATE INDEX idx_agent_runs_started ON agent_runs (started_at);
",
    ),
];

/// Open the `SQLite` database at `path`, apply connection pragmas, and run any
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

/// Create the `schema_version` bookkeeping table (DATABASE.md §7.7) and apply
/// any pending migrations.
fn migrate(conn: &mut Connection) -> Result<(), DatabaseError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (\
             version INTEGER PRIMARY KEY CHECK (version > 0),\
             applied_at INTEGER NOT NULL CHECK (applied_at > 0)\
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
        if version == 5 {
            // v5 rebuilds agent_runs, a parent of agent_steps; SQLite cannot
            // hold the FK while dropping the parent, and PRAGMA foreign_keys
            // cannot be toggled inside the transaction that apply_migration
            // opens. The toggle is therefore applied here, outside the tx.
            conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
            let result = apply_migration(conn, version, sql);
            // Restore enforcement for all subsequent work.
            match conn.execute_batch("PRAGMA foreign_keys=ON;") {
                Ok(()) => {}
                Err(restore_err) => {
                    if result.is_ok() {
                        return Err(restore_err.into());
                    }
                }
            }
            result?;
        } else {
            apply_migration(conn, version, sql)?;
        }
    }

    Ok(())
}

/// Execute one migration atomically.
///
/// The migration SQL and its `schema_version` bookkeeping insert commit
/// together and roll back together on failure (DATABASE.md §4–§5), so a
/// failed migration can never leave a partial schema or a stale version row.
/// `version` is not validated here; callers apply pending migrations in order.
fn apply_migration(conn: &mut Connection, version: i64, sql: &str) -> Result<(), DatabaseError> {
    let applied_at = now_millis();
    let tx = conn.transaction()?;
    tx.execute_batch(sql)?;
    tx.execute(
        "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
        params![version, applied_at],
    )?;
    tx.commit()?;
    log::info!("applied database migration v{version}");
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
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

/// Shared application-wide `SQLite` connection.
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

/// Open an in-memory database with production pragmas and the full migration
/// set applied. Test-only convenience for modules that exercise persistence
/// against the documented schema without the Tauri runtime.
#[cfg(test)]
pub(crate) fn in_memory_database() -> Database {
    let mut conn = Connection::open_in_memory().expect("open in-memory database");
    configure(&conn).expect("configure in-memory connection");
    migrate(&mut conn).expect("apply migrations");
    Database::new(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::search::LocalSearchService;
    use crate::infrastructure::repository::conversations::ConversationRepository;
    use crate::infrastructure::repository::messages::MessageRepository;
    use crate::infrastructure::repository::prompts::PromptRepository;

    /// Open an in-memory connection with the same pragmas and migrations as a
    /// production startup ([`open`]).
    fn in_memory_migrated() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory database");
        configure(&conn).expect("configure connection pragmas");
        migrate(&mut conn).expect("apply migrations");
        conn
    }

    /// Whether `name` exists in `sqlite_master` under the given object type.
    fn schema_object_exists(conn: &Connection, name: &str, object_type: &str) -> bool {
        let found: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE name = ?1 AND type = ?2",
                params![name, object_type],
                |row| row.get(0),
            )
            .ok();
        found.is_some()
    }

    /// Applied migration versions in ascending order.
    fn schema_version_rows(conn: &Connection) -> Vec<i64> {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_version ORDER BY version")
            .expect("prepare schema_version query");
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("query schema_version");
        rows.collect::<std::result::Result<Vec<i64>, _>>()
            .expect("collect schema versions")
    }

    #[test]
    fn fresh_database_receives_the_complete_documented_schema() {
        let conn = in_memory_migrated();

        for table in [
            "agent_runs",
            "agent_steps",
            "app_settings",
            "attachments",
            "conversations",
            "messages",
            "prompts",
            "providers",
        ] {
            assert!(
                schema_object_exists(&conn, table, "table"),
                "missing documented table {table}"
            );
        }
        for index in ["conversations_fts", "messages_fts", "prompts_fts"] {
            assert!(
                schema_object_exists(&conn, index, "table"),
                "missing FTS5 index {index}"
            );
        }
        for index in [
            "idx_agent_runs_conversation",
            "idx_agent_runs_started",
            "idx_agent_steps_run_seq",
            "idx_attachments_conversation",
            "idx_attachments_message",
            "idx_conversations_status_updated",
            "idx_messages_conversation_created",
            "idx_providers_name",
        ] {
            assert!(
                schema_object_exists(&conn, index, "index"),
                "missing functional index {index}"
            );
        }
        for trigger in [
            "conversations_fts_delete",
            "conversations_fts_insert",
            "conversations_fts_update",
            "conversations_touch_updated_at",
            "messages_fts_delete",
            "messages_fts_insert",
            "messages_fts_update",
            "prompts_fts_delete",
            "prompts_fts_insert",
            "prompts_fts_update",
            "prompts_touch_updated_at",
        ] {
            assert!(
                schema_object_exists(&conn, trigger, "trigger"),
                "missing trigger {trigger}"
            );
        }

        assert_eq!(schema_version_rows(&conn), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn migration_state_is_recorded_correctly() {
        let conn = in_memory_migrated();

        assert_eq!(schema_version_rows(&conn), vec![1, 2, 3, 4, 5]);

        let applied_at: i64 = conn
            .query_row(
                "SELECT applied_at FROM schema_version ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("read newest applied_at");
        assert!(applied_at > 0, "migration timestamp must be recorded");

        let version_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
            .expect("count schema_version rows");
        assert_eq!(version_count, 5, "one row per applied migration");
    }

    #[test]
    fn re_running_migrations_is_a_no_op() {
        let mut conn = in_memory_migrated();
        assert_eq!(schema_version_rows(&conn), vec![1, 2, 3, 4, 5]);

        migrate(&mut conn).expect("a second migration run must succeed");

        assert_eq!(schema_version_rows(&conn), vec![1, 2, 3, 4, 5]);
        // The no-op run created or dropped nothing.
        assert!(schema_object_exists(&conn, "conversations", "table"));
        assert!(schema_object_exists(&conn, "conversations_fts", "table"));
        assert!(schema_object_exists(
            &conn,
            "conversations_fts_insert",
            "trigger"
        ));
    }

    #[test]
    fn failed_migration_rolls_back_the_whole_transaction() {
        let mut conn = in_memory_migrated();

        // A single migration batch: one valid statement followed by a syntax
        // error. Without a transaction the `partial_table` would survive.
        let err = apply_migration(
            &mut conn,
            99,
            "CREATE TABLE partial_table (id INTEGER PRIMARY KEY);\n\
             CREATE TABLE broken (oops;",
        );
        assert!(err.is_err(), "the failing migration must be reported");

        assert!(
            !schema_object_exists(&conn, "partial_table", "table"),
            "the valid part of the failed migration must roll back"
        );
        assert_eq!(schema_version_rows(&conn), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn foreign_key_constraints_are_enforced() {
        let conn = in_memory_migrated();

        let enabled: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("read foreign_keys pragma");
        assert_eq!(enabled, 1, "foreign key enforcement is enabled");

        // A message must belong to an existing conversation.
        let orphan_message = conn.execute(
            "INSERT INTO messages (conversation_id, role, content) VALUES (999, 'user', 'x')",
            [],
        );
        assert!(orphan_message.is_err(), "orphan message must be rejected");

        // An attachment must belong to an existing conversation.
        let orphan_attachment = conn.execute(
            "INSERT INTO attachments (conversation_id, file_name, file_path) \
             VALUES (999, 'f.txt', '/tmp/f.txt')",
            [],
        );
        assert!(
            orphan_attachment.is_err(),
            "orphan attachment must be rejected"
        );

        // A message must not reference a provider that does not exist.
        let unknown_provider = conn.execute(
            "INSERT INTO messages (conversation_id, role, content, provider_id) \
             VALUES (1, 'user', 'x', 500)",
            [],
        );
        assert!(
            unknown_provider.is_err(),
            "unknown provider must be rejected"
        );

        // Valid references are accepted, and deleting a conversation cascades
        // to its messages and attachments (DATABASE.md §9).
        conn.execute("INSERT INTO conversations (title) VALUES ('c')", [])
            .expect("insert conversation");
        let conv_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO messages (conversation_id, role, content) VALUES (?1, 'user', 'hey')",
            [conv_id],
        )
        .expect("message for an existing conversation is accepted");
        let msg_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO attachments (conversation_id, message_id, file_name, file_path) \
             VALUES (?1, ?2, 'f.txt', '/tmp/f.txt')",
            params![conv_id, msg_id],
        )
        .expect("attachment for an existing conversation and message is accepted");

        conn.execute("DELETE FROM conversations WHERE id = ?1", [conv_id])
            .expect("delete conversation");
        let messages_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE conversation_id = ?1",
                [conv_id],
                |row| row.get(0),
            )
            .expect("count messages after cascade");
        let attachments_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attachments WHERE conversation_id = ?1",
                [conv_id],
                |row| row.get(0),
            )
            .expect("count attachments after cascade");
        assert_eq!(messages_left, 0, "conversation delete cascades messages");
        assert_eq!(
            attachments_left, 0,
            "conversation delete cascades attachments"
        );
    }

    #[test]
    fn documented_check_constraints_are_enforced() {
        let conn = in_memory_migrated();

        // conversations: non-empty title within 500 chars, valid status.
        assert!(conn
            .execute("INSERT INTO conversations (title) VALUES ('')", [])
            .is_err());
        conn.execute("INSERT INTO conversations (title) VALUES ('ok')", [])
            .expect("valid conversation accepted");
        let conv_id = conn.last_insert_rowid();
        assert!(conn
            .execute(
                "UPDATE conversations SET status = 'broken' WHERE id = ?1",
                [conv_id],
            )
            .is_err());

        // messages: role enumeration and non-empty content.
        conn.execute(
            "INSERT INTO messages (conversation_id, role, content) VALUES (?1, 'user', 'x')",
            [conv_id],
        )
        .expect("valid message accepted");
        assert!(conn
            .execute(
                "INSERT INTO messages (conversation_id, role, content) VALUES (?1, 'system', 'x')",
                [conv_id],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO messages (conversation_id, role, content) VALUES (?1, 'user', '')",
                [conv_id],
            )
            .is_err());

        // prompts: non-empty title, content within 10000 chars.
        assert!(conn
            .execute("INSERT INTO prompts (title, content) VALUES ('', 'x')", [])
            .is_err());
        conn.execute("INSERT INTO prompts (title, content) VALUES ('t', 'y')", [])
            .expect("valid prompt accepted");
        let long_content = "x".repeat(10_001);
        assert!(conn
            .execute(
                "INSERT INTO prompts (title, content) VALUES ('t', ?1)",
                [long_content],
            )
            .is_err());

        // attachments: non-empty file_name within 255 chars, non-empty
        // file_path, non-negative file size, and the documented boundaries.
        assert!(conn
            .execute(
                "INSERT INTO attachments (conversation_id, file_name, file_path) \
                 VALUES (?1, '', '/tmp/f')",
                [conv_id],
            )
            .is_err());
        let long_name = "n".repeat(256);
        assert!(conn
            .execute(
                "INSERT INTO attachments (conversation_id, file_name, file_path) \
                 VALUES (?1, ?2, '/tmp/f')",
                params![conv_id, long_name],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO attachments (conversation_id, file_name, file_path, file_size_bytes) \
                 VALUES (?1, 'f.txt', '', -1)",
                [conv_id],
            )
            .is_err());
        let boundary_name = "n".repeat(255);
        conn.execute(
            "INSERT INTO attachments (conversation_id, file_name, file_path, file_size_bytes) \
             VALUES (?1, ?2, '/tmp/f', 0)",
            params![conv_id, boundary_name],
        )
        .expect("documented boundary values are accepted");

        // providers: unique name, non-empty display_name.
        assert!(conn
            .execute(
                "INSERT INTO providers (name, display_name) VALUES ('openai', '')",
                []
            )
            .is_err());
        conn.execute(
            "INSERT INTO providers (name, display_name) VALUES ('openai', 'OpenAI')",
            [],
        )
        .expect("valid provider accepted");
        assert!(conn
            .execute(
                "INSERT INTO providers (name, display_name) VALUES ('openai', 'Duplicate')",
                [],
            )
            .is_err());

        // app_settings: non-empty key, value within 10000 chars.
        assert!(conn
            .execute("INSERT INTO app_settings (key, value) VALUES ('', 'v')", [])
            .is_err());
        let long_value = "v".repeat(10_001);
        assert!(conn
            .execute(
                "INSERT INTO app_settings (key, value) VALUES ('k', ?1)",
                [long_value],
            )
            .is_err());
    }

    #[test]
    fn conversation_fts_index_stays_in_sync() {
        let conn = in_memory_migrated();
        let db = Database::new(conn);
        let conversations = ConversationRepository::new(&db);
        let search = LocalSearchService::new(&db);

        let id = conversations
            .create("Q3 roadmap planning", "active")
            .expect("create conversation");
        assert!(
            search
                .search("roadmap")
                .expect("search")
                .conversations
                .iter()
                .any(|c| c.id == id),
            "inserted conversation is searchable by title"
        );

        conversations
            .update(id, "Release logistics", "active")
            .expect("rename conversation");
        assert!(
            !search
                .search("roadmap")
                .expect("search")
                .conversations
                .iter()
                .any(|c| c.id == id),
            "renamed conversation no longer matches its old title"
        );
        assert!(
            search
                .search("logistics")
                .expect("search")
                .conversations
                .iter()
                .any(|c| c.id == id),
            "renamed conversation matches its new title"
        );

        conversations.delete(id).expect("delete conversation");
        assert!(
            !search
                .search("logistics")
                .expect("search")
                .conversations
                .iter()
                .any(|c| c.id == id),
            "deleted conversation is removed from the title index"
        );
    }

    #[test]
    fn message_fts_index_stays_in_sync() {
        let conn = in_memory_migrated();
        let db = Database::new(conn);
        let conversations = ConversationRepository::new(&db);
        let messages = MessageRepository::new(&db);
        let search = LocalSearchService::new(&db);

        let conv_id = conversations
            .create("General", "active")
            .expect("create conversation");
        messages
            .create(
                conv_id,
                "user",
                "the launch strategy needs review",
                None,
                None,
            )
            .expect("create message");

        let hits = search.search("strategy").expect("search");
        assert!(
            hits.message_matches
                .iter()
                .any(|m| m.conversation_id == conv_id),
            "message content is searchable and names its conversation"
        );

        // Deleting the conversation removes its messages and their index rows.
        conversations.delete(conv_id).expect("delete conversation");
        assert!(
            search
                .search("strategy")
                .expect("search")
                .message_matches
                .is_empty(),
            "cascaded message deletion is reflected in the content index"
        );
    }

    #[test]
    fn prompt_fts_index_stays_in_sync() {
        let conn = in_memory_migrated();
        let db = Database::new(conn);
        let prompts = PromptRepository::new(&db);
        let search = LocalSearchService::new(&db);

        let id = prompts
            .create("Code review checklist", "Go through the diff.")
            .expect("create prompt");
        assert!(
            search
                .search("checklist")
                .expect("search")
                .prompts
                .iter()
                .any(|p| p.id == id),
            "prompt title is searchable"
        );
        assert!(
            search
                .search("diff")
                .expect("search")
                .prompts
                .iter()
                .any(|p| p.id == id),
            "prompt content is searchable"
        );

        prompts
            .update(id, "Code review checklist", "Verify the closing brace")
            .expect("update prompt");
        assert!(
            !search
                .search("diff")
                .expect("search")
                .prompts
                .iter()
                .any(|p| p.id == id),
            "updated prompt no longer matches its old content"
        );
        assert!(
            search
                .search("closing")
                .expect("search")
                .prompts
                .iter()
                .any(|p| p.id == id),
            "updated prompt matches its new content"
        );

        prompts.delete(id).expect("delete prompt");
        assert!(
            search.search("closing").expect("search").prompts.is_empty(),
            "deleted prompt is removed from the prompt index"
        );
    }

    #[test]
    fn updated_at_trigger_touches_the_timestamp_on_rename() {
        let conn = in_memory_migrated();
        conn.execute("INSERT INTO conversations (title) VALUES ('old title')", [])
            .expect("insert conversation");
        let id = conn.last_insert_rowid();
        // Rewind created_at so a successful trigger write is observable while
        // the `updated_at >= created_at` CHECK remains satisfiable.
        conn.execute(
            "UPDATE conversations SET created_at = 1 WHERE id = ?1",
            [id],
        )
        .expect("rewind created_at");
        conn.execute(
            "UPDATE conversations SET title = 'new title' WHERE id = ?1",
            [id],
        )
        .expect("rename conversation");

        let (created_at, updated_at): (i64, i64) = conn
            .query_row(
                "SELECT created_at, updated_at FROM conversations WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read timestamps");
        assert_eq!(created_at, 1);
        assert!(
            updated_at > created_at,
            "rename must refresh updated_at through the trigger"
        );
    }
    // -----------------------------------------------------------------------
    // Agent run persistence (DATABASE.md §7.8, §7.9; Task 4.2)
    // -----------------------------------------------------------------------

    #[test]
    fn agent_run_tables_enforce_foreign_keys_and_cascades() {
        let db = in_memory_database();
        let conn = db.lock().expect("lock connection");

        // A step must belong to an existing run.
        let orphan_step = conn.execute(
            "INSERT INTO agent_steps (run_id, seq, kind) VALUES (999, 1, 'model_turn')",
            [],
        );
        assert!(orphan_step.is_err(), "orphan agent_steps must be rejected");

        // A run must belong to an existing conversation when linked.
        let orphan_run = conn.execute(
            "INSERT INTO agent_runs (conversation_id, model, mode) \
             VALUES (999, 'm', 'supervised')",
            [],
        );
        assert!(orphan_run.is_err(), "orphan agent_runs must be rejected");

        // conversation_id = NULL is allowed until the Task 5.1 IPC layer
        // wires runs to conversations (DATABASE.md §7.8).
        conn.execute(
            "INSERT INTO agent_runs (model, mode) VALUES ('m', 'supervised')",
            [],
        )
        .expect("NULL conversation_id run accepted");
        let run_id = conn.last_insert_rowid();

        for seq in 1..=2 {
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind, tool_name) \
                 VALUES (?1, ?2, 'tool_call', 'write_file')",
                params![run_id, seq],
            )
            .expect("append step");
        }

        // Deleting a run cascades to its steps (DATABASE.md §7.8, §9).
        conn.execute("DELETE FROM agent_runs WHERE id = ?1", [run_id])
            .expect("delete run");
        let steps_left: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_steps", [], |row| row.get(0))
            .expect("count steps after run cascade");
        assert_eq!(steps_left, 0, "run delete cascades steps");
    }

    #[test]
    fn deleting_a_conversation_cascades_agent_runs_and_steps() {
        let db = in_memory_database();
        let conn = db.lock().expect("lock connection");

        conn.execute("INSERT INTO conversations (title) VALUES ('c')", [])
            .expect("insert conversation");
        let conv_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO agent_runs (conversation_id, model, mode) \
             VALUES (?1, 'm', 'supervised')",
            [conv_id],
        )
        .expect("linked run accepted");
        let run_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO agent_steps (run_id, seq, kind) VALUES (?1, 1, 'model_turn')",
            [run_id],
        )
        .expect("append step");

        // D50 privacy doctrine: removing a conversation atomically removes
        // its agent runs and, through them, their steps (DATABASE.md §9).
        conn.execute("DELETE FROM conversations WHERE id = ?1", [conv_id])
            .expect("delete conversation");
        let runs_left: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_runs", [], |row| row.get(0))
            .expect("count runs after conversation cascade");
        let steps_left: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_steps", [], |row| row.get(0))
            .expect("count steps after conversation cascade");
        assert_eq!(runs_left, 0, "conversation delete cascades agent runs");
        assert_eq!(steps_left, 0, "conversation delete cascades agent steps");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn agent_tables_enforce_documented_check_constraints() {
        let db = in_memory_database();
        let conn = db.lock().expect("lock connection");

        conn.execute(
            "INSERT INTO agent_runs (model, mode) VALUES ('m', 'supervised')",
            [],
        )
        .expect("baseline run accepted");
        let run_id = conn.last_insert_rowid();

        // agent_runs: non-empty model, mode enumeration, run status
        // enumeration, positive timestamps, non-negative totals.
        assert!(
            conn.execute(
                "INSERT INTO agent_runs (model, mode) VALUES ('', 'supervised')",
                [],
            )
            .is_err(),
            "empty model must be rejected"
        );
        assert!(
            conn.execute(
                "INSERT INTO agent_runs (model, mode) VALUES ('m', 'chaos')",
                [],
            )
            .is_err(),
            "unknown mode must be rejected"
        );
        assert!(
            conn.execute(
                "UPDATE agent_runs SET status = 'warp' WHERE id = ?1",
                [run_id],
            )
            .is_err(),
            "unknown run status must be rejected"
        );
        assert!(
            conn.execute(
                "UPDATE agent_runs SET total_steps = -1 WHERE id = ?1",
                [run_id],
            )
            .is_err(),
            "negative total_steps must be rejected"
        );
        assert!(
            conn.execute(
                "UPDATE agent_runs SET finished_at = 0 WHERE id = ?1",
                [run_id],
            )
            .is_err(),
            "non-positive finished_at must be rejected"
        );
        assert!(
            conn.execute(
                "UPDATE agent_runs SET started_at = 0 WHERE id = ?1",
                [run_id],
            )
            .is_err(),
            "non-positive started_at must be rejected"
        );

        // agent_steps: kind enumeration, seq >= 1, step status enumeration,
        // non-empty tool_name, non-negative duration_ms.
        let step_base = "INSERT INTO agent_steps (run_id, seq, kind) VALUES (?1, ?2, ?3)";
        conn.execute(step_base, params![run_id, 1, "model_turn"])
            .expect("valid step accepted");
        assert!(
            conn.execute(step_base, params![run_id, 2, "divination"])
                .is_err(),
            "unknown step kind must be rejected"
        );
        assert!(
            conn.execute(step_base, params![run_id, 0, "model_turn"])
                .is_err(),
            "seq < 1 must be rejected"
        );
        let duplicate = conn.execute(step_base, params![run_id, 1, "tool_call"]);
        assert!(
            duplicate.is_err(),
            "UNIQUE(run_id, seq) must reject a duplicate seq"
        );
        assert!(
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind, status) \
                 VALUES (?1, 2, 'tool_call', 'transcended')",
                [run_id],
            )
            .is_err(),
            "unknown step status must be rejected"
        );
        assert!(
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind, tool_name) \
                 VALUES (?1, 3, 'tool_call', '')",
                [run_id],
            )
            .is_err(),
            "empty tool_name must be rejected"
        );
        assert!(
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind, duration_ms) \
                 VALUES (?1, 4, 'tool_call', -5)",
                [run_id],
            )
            .is_err(),
            "negative duration_ms must be rejected"
        );
    }
    #[test]
    fn v5_preserves_v4_rows_and_enables_spend_limit_exceeded() {
        // Build a v4-only DB: apply migrations 1..=4 manually, insert rows, then migrate to v5.
        let mut conn = Connection::open_in_memory().expect("open in-memory");
        configure(&conn).expect("configure");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY CHECK (version > 0), applied_at INTEGER NOT NULL CHECK (applied_at > 0));",
        )
        .expect("create schema_version");
        // Apply only v1..v4
        for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v <= 4) {
            apply_migration(&mut conn, version, sql).expect("apply v1..v4");
        }
        // Seed a v4-shaped run and step
        conn.execute(
            "INSERT INTO agent_runs (model, mode, status) VALUES ('m', 'supervised', 'completed')",
            [],
        )
        .expect("seed run");
        let run_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO agent_steps (run_id, seq, kind) VALUES (?1, 1, 'model_turn')",
            [run_id],
        )
        .expect("seed step");
        // Migrate to v5 (and any later)
        migrate(&mut conn).expect("migrate to v5");
        // Schema version is 5
        assert_eq!(schema_version_rows(&conn), vec![1, 2, 3, 4, 5]);
        // Row preserved, new columns NULL for pre-v5 rows
        let (status, spent, limit): (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT status, spent_micro_usd, limit_micro_usd FROM agent_runs WHERE id = ?1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read preserved run");
        assert_eq!(status, "completed");
        assert_eq!(spent, None, "pre-v5 spent is NULL");
        assert_eq!(limit, None, "pre-v5 limit is NULL");
        // Step preserved
        let steps: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_steps WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .expect("count steps");
        assert_eq!(steps, 1);
        // FK still ON after rebuild
        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("fk pragma");
        assert_eq!(fk_on, 1, "FK must be ON after v5");
        // New status is now accepted
        conn.execute(
            "INSERT INTO agent_runs (model, mode, status) VALUES ('m', 'supervised', 'spend_limit_exceeded')",
            [],
        )
        .expect("new status accepted");
        // New columns accept values and reject negatives
        let new_id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE agent_runs SET spent_micro_usd = 123, limit_micro_usd = 456 WHERE id = ?1",
            [new_id],
        )
        .expect("spend columns accept non-negative");
        let bad = conn.execute(
            "UPDATE agent_runs SET spent_micro_usd = -1 WHERE id = ?1",
            [new_id],
        );
        assert!(bad.is_err(), "negative spent must be rejected");
        let bad2 = conn.execute(
            "UPDATE agent_runs SET limit_micro_usd = -1 WHERE id = ?1",
            [new_id],
        );
        assert!(bad2.is_err(), "negative limit must be rejected");
        // Unknown status still rejected
        let bad_status = conn.execute(
            "INSERT INTO agent_runs (model, mode, status) VALUES ('m', 'supervised', 'warp')",
            [],
        );
        assert!(bad_status.is_err(), "unknown status still rejected");
    }
}
