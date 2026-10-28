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
//! `updated_at` maintenance triggers (v3), the agent run persistence tables
//! `agent_runs` / `agent_steps` (v4, Task 4.2), the spend-guard columns and
//! widened status CHECK (v5, Task 4.3), and the per-folder
//! `conversations.workspace_root` column (v6), the persistent permission
//! rules table `permission_rules` plus the `agent_steps` provenance columns
//! (v7, M1-core), the task-manager tables `agent_tasks` /
//! `agent_task_steps` (v8, task manager + autonomous mode), the monotonic
//! `updated_at` touch triggers (v12, NEX-DB-001/003), the cross-column
//! lifecycle invariants (v13, NEX-DB-002/004/006), and the per-turn
//! `agent_steps` token-usage columns (v14, NEX-AGENT-005).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::ffi::ErrorCode;
use rusqlite::{params, Connection};

/// Busy budget shared by the `SQLite` pragma and the cross-process migration
/// leadership lock. Past this budget the caller gets an honest
/// [`DatabaseError::Busy`] instead of an unbounded wait or a silent
/// read-only fallback.
pub const BUSY_TIMEOUT_MS: u64 = 5000;

/// Errors raised while opening or migrating the database.
#[derive(Debug)]
pub enum DatabaseError {
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
    /// Recoverable: [`Database::lock`] clears the poison and resets the
    /// connection before returning this, so one panicking holder fails only
    /// its observing call instead of bricking every later operation.
    Lock(String),
    /// The `schema_version` bookkeeping rows are not the contiguous
    /// `1..=MAX(version)` run the forward-only runner applies (NEX-DB-007):
    /// a version was skipped or removed, so continuing would silently
    /// mis-migrate. Fails loud at startup instead; carries only the first
    /// missing version, never SQL or content.
    SchemaGap {
        /// First migration version missing from the bookkeeping table.
        missing: i64,
    },
    /// Another process held the write/migration lock past the
    /// [`BUSY_TIMEOUT_MS`] budget. Deliberately a fixed message: callers get
    /// an honest "retry when it finishes" instead of an unbounded wait, a
    /// silent read-only downgrade, or a guess about the other process.
    Busy,
    /// A filesystem operation around the database (migration-leadership or
    /// instance lock) failed.
    Io(io::Error),
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
            Self::SchemaGap { missing } => write!(
                f,
                "database schema is missing migration version {missing}: \
                 the migration history is incomplete and startup cannot continue"
            ),
            Self::Busy => write!(
                f,
                "database is busy: another Nexora process held the write lock \
                 longer than 5s; retry when it finishes"
            ),
            Self::Io(err) => write!(f, "storage i/o error: {err}"),
        }
    }
}

impl std::error::Error for DatabaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::SchemaTooNew { .. } | Self::Lock(_) | Self::SchemaGap { .. } | Self::Busy => None,
        }
    }
}

/// Classify a storage failure into the shared, secret-free error taxonomy.
///
/// Both types are local to this crate, which is why the mapping lives here
/// (the desktop cannot implement `From<DatabaseError> for CoreError` — both
/// are foreign there). Every shell therefore assigns the same category and
/// the same fixed user-facing text; only the fixed tag goes to the log.
impl From<DatabaseError> for crate::error::CoreError {
    fn from(err: DatabaseError) -> Self {
        let tag = match &err {
            DatabaseError::Sqlite(_) => "sqlite",
            DatabaseError::SchemaTooNew { .. } => "schema_too_new",
            DatabaseError::Lock(_) => "lock",
            DatabaseError::SchemaGap { .. } => "schema_gap",
            DatabaseError::Busy => "busy",
            DatabaseError::Io(_) => "io",
        };
        log::error!("database operation failed (kind: {tag})");
        Self::new(
            crate::error::CoreErrorKind::Database,
            "a database operation failed; no data was changed".to_string(),
        )
    }
}

impl From<rusqlite::Error> for DatabaseError {
    fn from(err: rusqlite::Error) -> Self {
        // Busy and Locked are not generic SQLite failures: they mean another
        // connection/process still holds the write lock past `busy_timeout`.
        // Surface that as the honest typed `Busy` instead of a raw SQL text
        // the UI would have to guess about.
        match &err {
            rusqlite::Error::SqliteFailure(code, _)
                if matches!(
                    code.code,
                    ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
                ) =>
            {
                Self::Busy
            }
            _ => Self::Sqlite(err),
        }
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
/// - v6: the per-folder history column `conversations.workspace_root`
///   (`TEXT NULL CHECK (length <= 1024)`, no FK): the canonical workspace root
///   a conversation belongs to, `NULL` for pre-picker rows.
/// - v7: the persistent permission rules table `permission_rules` (M1-core)
///   with its documented columns, defaults, CHECKs and UNIQUE, seeded with
///   the coding shell-ask row, plus the `agent_steps` provenance columns
///   `rule_id` / `group_key` / `decided_by` via a validated rebuild.
/// - v8: the task-manager tables `agent_tasks` and `agent_task_steps` (task
///   manager + autonomous mode): user-defined task lists whose steps the
///   autonomous loop executes through the existing agent run path.
/// - v9: the local-only usage ledger `usage_ledger` (privacy center): per-day
///   counters of whitelisted event kinds — no content columns by construction
///   (only the fixed `kind` vocabulary plus integer `day` / `count`), kept to
///   the last 90 days by the recording path. There is no upload anywhere.
/// - v10: the technical-debt backlog table `debt_items` (debt backlog):
///   manual entries plus idempotent imports of `repo_audit` findings.
/// - v11: the kanban edit-lock table `task_locks` (P2 kanban+locks): at most
///   one row per task (`task_id` PK, CASCADE on task delete), carrying the
///   holder label, an opaque token, and a mandatory deadline
///   (`CHECK (expires_at > acquired_at)` — no eternal lock; steal-on-expiry
///   is enforced by the repository, never by the schema).
/// - v12: monotonic `updated_at` touch triggers (NEX-DB-001/003): the v3
///   `conversations_touch_updated_at` / `prompts_touch_updated_at` triggers
///   wrote `updated_at = unixepoch()`, so any row whose `created_at` lies in
///   the future (a within-skew import, an NTP/snapshot backward clock move)
///   failed its next update against the `updated_at >= created_at` CHECK and
///   bricked the row. The triggers now write
///   `max(unixepoch(), created_at)`, keeping the CHECK satisfiable; the
///   repository-side `updated_at` writes on `conversations` / `agent_tasks`
///   use the same expression. The cross-column CHECKs are kept — they still
///   reject genuinely inverted timestamps at insert time.
/// - v13: cross-column lifecycle invariants (NEX-DB-002/004/006) via four
///   validated table rebuilds — the `messages` / `attachments` composite link
///   (with repair of pre-existing cross-conversation rows to the draft
///   state), the `agent_tasks` progress CHECK (with clamping), and the
///   `agent_runs` terminal-status / timestamp-ordering CHECKs (with minimal
///   honest repairs), plus `total_steps` normalization to the child-row
///   count.
/// - v14: per-turn token-usage columns on `agent_steps` (NEX-AGENT-005):
///   NULL-able `input_tokens` / `output_tokens` (`CHECK >= 0`), populated
///   from the provider-reported `response.usage` by the runner and read by
///   `application::context_stats`. A plain `ALTER TABLE ... ADD COLUMN`
///   (no rebuild: no FK/UNIQUE involved); pre-existing rows read as NULL
///   (honest unknown — the meter renders "n/a" for those turns, never a
///   fabricated zero).
pub const MIGRATIONS: &[(i64, &str)] = &[
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
    // v6 — per-folder chat history (workspace folder picker 1.3.0). The new
    // `conversations.workspace_root` column records the canonical workspace
    // root a conversation belongs to (`NULL` for pre-picker rows; no FK, the
    // setting owns the root). Length bound mirrors the workspace guard
    // (`application/workspace.rs::WORKSPACE_ROOT_MAX_LEN`).
    (
        6,
        "ALTER TABLE conversations ADD COLUMN workspace_root TEXT \
         CHECK (workspace_root IS NULL OR length(workspace_root) <= 1024);",
    ),
    // v7 — persistent permission rules (M1-core) + agent_steps provenance.
    // `permission_rules` holds ordered, revocable rules; `agent_steps` gains
    // `rule_id` / `group_key` / `decided_by` via a table rebuild (the v5
    // precedent: SQLite cannot ADD a REFERENCES column with the needed
    // constraints through a bare ALTER, and the rebuild keeps the FK graph
    // validated). Existing step rows keep NULL provenance.
    (
        7,
        r"CREATE TABLE permission_rules (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    preset TEXT NOT NULL CHECK (preset IN ('coding', 'document', '*')),
    tool_pattern TEXT NOT NULL CHECK (length(tool_pattern) > 0 AND length(tool_pattern) <= 64),
    path_pattern TEXT CHECK (path_pattern IS NULL OR (length(path_pattern) > 0 AND length(path_pattern) <= 1024)),
    effect TEXT NOT NULL CHECK (effect IN ('allow', 'ask', 'deny')),
    priority INTEGER NOT NULL DEFAULT 100 CHECK (priority >= 0),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (created_at > 0),
    UNIQUE (preset, tool_pattern, path_pattern)
);

INSERT INTO permission_rules (preset, tool_pattern, path_pattern, effect, priority)
    VALUES ('coding', 'execute_command', '*', 'ask', 50);

CREATE TABLE agent_steps_new (
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
    rule_id INTEGER NULL REFERENCES permission_rules(id) ON DELETE SET NULL,
    group_key TEXT NULL CHECK (group_key IS NULL OR length(group_key) <= 256),
    decided_by TEXT NULL CHECK (decided_by IS NULL OR decided_by IN ('user', 'rule', 'system')),
    UNIQUE (run_id, seq)
);

INSERT INTO agent_steps_new
    (id, run_id, seq, kind, tool_name, arguments, observation, status, started_at, duration_ms)
    SELECT id, run_id, seq, kind, tool_name, arguments, observation, status, started_at, duration_ms
    FROM agent_steps;

DROP TABLE agent_steps;
ALTER TABLE agent_steps_new RENAME TO agent_steps;

CREATE INDEX idx_agent_steps_run_seq
    ON agent_steps (run_id, seq);
",
    ),
    // v8 — task manager + autonomous mode: user-defined task lists
    // (`agent_tasks`) with agent-executable steps (`agent_task_steps`). New
    // tables only (no rebuild): `agent_tasks` links the backing conversation
    // (CASCADE, D50) and the latest step's agent run (SET NULL, history
    // survives); `agent_task_steps` links its task (CASCADE) and its own step
    // run (SET NULL). `updated_at` is maintained by the repository's explicit
    // writes (no trigger: only title/status/report writes touch it).
    (
        8,
        r"CREATE TABLE agent_tasks (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    title TEXT NOT NULL CHECK (length(title) > 0 AND length(title) <= 200),
    description TEXT
        CHECK (description IS NULL OR length(description) <= 4000),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled')),
    conversation_id INTEGER
        CHECK (conversation_id IS NULL OR conversation_id > 0)
        REFERENCES conversations (id) ON DELETE CASCADE,
    provider TEXT
        CHECK (provider IS NULL OR (length(provider) > 0 AND length(provider) <= 100)),
    model TEXT
        CHECK (model IS NULL OR (length(model) > 0 AND length(model) <= 200)),
    max_steps INTEGER NOT NULL DEFAULT 25 CHECK (max_steps >= 1 AND max_steps <= 25),
    current_step INTEGER NOT NULL DEFAULT 0 CHECK (current_step >= 0),
    total_steps INTEGER NOT NULL DEFAULT 0 CHECK (total_steps >= 0),
    report TEXT,
    run_id INTEGER
        CHECK (run_id IS NULL OR run_id > 0)
        REFERENCES agent_runs (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (created_at > 0),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        CHECK (updated_at >= created_at)
);

CREATE TABLE agent_task_steps (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    task_id INTEGER NOT NULL
        CHECK (task_id > 0)
        REFERENCES agent_tasks (id) ON DELETE CASCADE,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    title TEXT NOT NULL CHECK (length(title) > 0 AND length(title) <= 500),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped', 'cancelled')),
    result TEXT,
    run_id INTEGER
        CHECK (run_id IS NULL OR run_id > 0)
        REFERENCES agent_runs (id) ON DELETE SET NULL,
    started_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (started_at > 0),
    finished_at INTEGER CHECK (finished_at IS NULL OR finished_at > 0),
    UNIQUE (task_id, seq)
);

CREATE INDEX idx_agent_tasks_status_updated
    ON agent_tasks (status, updated_at);

CREATE INDEX idx_agent_task_steps_task_seq
    ON agent_task_steps (task_id, seq);
",
    ),
    // v9 — local-only usage ledger (privacy center). Counts only: the fixed
    // `kind` vocabulary plus integer day-bucket / counter columns. There is
    // deliberately NO text content column — titles, prompts, message bodies,
    // URLs, and error text can never be recorded here (enforced by the
    // schema itself, not by caller discipline). Rows older than 90 days are
    // pruned by the recording path. Nothing on this path ever leaves the
    // machine: the ledger is read by `privacy_status` / `privacy_export`
    // and cleared by the user-confirmed `privacy_wipe` only.
    (
        9,
        r"CREATE TABLE usage_ledger (
    kind TEXT NOT NULL
        CHECK (kind IN ('agent_run', 'message', 'github_read', 'update_check')),
    day INTEGER NOT NULL CHECK (day > 0),
    count INTEGER NOT NULL CHECK (count >= 0),
    PRIMARY KEY (kind, day)
);

CREATE INDEX idx_usage_ledger_day
    ON usage_ledger (day);
",
    ),
    // v10 — technical-debt backlog (debt backlog): one row per tracked debt
    // item, either manually added (`source='manual'`) or imported from the
    // read-only repo-audit scanners (`source='audit'`). `status` tracks the
    // triage lifecycle (`open` → `accepted` → `fixed`, or `wontfix`);
    // `severity` reuses the audit `info` / `warning` vocabulary.
    // `audit_key` is the import identity (`kind<US>path<US>line`, NULL for
    // manual rows): UNIQUE so re-running the import skips rows it already
    // created (`INSERT OR IGNORE`), while NULL manual rows never collide
    // (SQLite treats NULL UNIQUE members as distinct). `updated_at` is
    // refreshed explicitly on every status write (no trigger: only the
    // status path touches it), keeping the `updated_at >= created_at`
    // monotonic discipline of the sibling tables.
    (
        10,
        r"CREATE TABLE debt_items (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    title TEXT NOT NULL CHECK (length(title) > 0 AND length(title) <= 200),
    source TEXT NOT NULL CHECK (source IN ('audit', 'manual')),
    severity TEXT NOT NULL DEFAULT 'info'
        CHECK (severity IN ('info', 'warning')),
    status TEXT NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'accepted', 'fixed', 'wontfix')),
    location TEXT
        CHECK (location IS NULL OR (length(location) > 0 AND length(location) <= 1024)),
    note TEXT
        CHECK (note IS NULL OR length(note) <= 4000),
    audit_key TEXT UNIQUE
        CHECK (audit_key IS NULL OR length(audit_key) <= 2048),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (created_at > 0),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        CHECK (updated_at >= created_at)
);

CREATE INDEX idx_debt_items_status_updated
    ON debt_items (status, updated_at);
",
    ),
    // v11 — kanban edit locks (P2 kanban+locks). At most one lock row per
    // task: `task_id` is the PK (CASCADE so task delete drops its lock),
    // `holder` is a short surface label, `token` is the opaque proof the
    // mutating command must present, and `expires_at` is the mandatory
    // deadline (`CHECK (expires_at > acquired_at)` — a lock can never be
    // eternal; an expired row is stealable by the next acquirer).
    (
        11,
        r"CREATE TABLE task_locks (
    task_id INTEGER PRIMARY KEY CHECK (task_id > 0)
        REFERENCES agent_tasks (id) ON DELETE CASCADE,
    holder TEXT NOT NULL CHECK (length(holder) > 0 AND length(holder) <= 64),
    token TEXT NOT NULL UNIQUE CHECK (length(token) > 0 AND length(token) <= 128),
    acquired_at INTEGER NOT NULL CHECK (acquired_at > 0),
    expires_at INTEGER NOT NULL CHECK (expires_at > acquired_at)
);

CREATE INDEX idx_task_locks_expires
    ON task_locks (expires_at);
",
    ),
    // v12 — monotonic `updated_at` touch triggers (NEX-DB-001/003). Replaces
    // the v3 triggers, which stamped `updated_at = unixepoch()`: any row with
    // a future `created_at` (within-skew import, backward clock movement)
    // then failed its next title/status/content update against the
    // `updated_at >= created_at` CHECK. The new bodies stamp
    // `max(unixepoch(), created_at)` so the write can never go below
    // `created_at`. Drop + recreate (SQLite has no CREATE OR REPLACE
    // TRIGGER); no row data is touched.
    (
        12,
        r"DROP TRIGGER IF EXISTS conversations_touch_updated_at;

CREATE TRIGGER conversations_touch_updated_at
AFTER UPDATE OF title, status ON conversations
BEGIN
    UPDATE conversations
        SET updated_at = max((unixepoch()), created_at)
        WHERE id = old.id;
END;

DROP TRIGGER IF EXISTS prompts_touch_updated_at;

CREATE TRIGGER prompts_touch_updated_at
AFTER UPDATE OF title, content ON prompts
BEGIN
    UPDATE prompts
        SET updated_at = max((unixepoch()), created_at)
        WHERE id = old.id;
END;
",
    ),
    // v13 — cross-column invariants (NEX-DB-002/004/006). Four table rebuilds
    // (the v5/v7 precedent: create new → copy → drop → rename, FKs toggled
    // off outside the transaction by `migrate`):
    // - `messages` gains `UNIQUE (conversation_id, id)` so the attachment
    //   link can reference the (conversation, message) pair. Rows are
    //   identical; the messages FTS triggers and the conversation index are
    //   recreated (dropping the table drops its triggers/indexes; the FTS5
    //   content tables are untouched, so no reindex is needed).
    // - `attachments` gains a composite
    //   `FOREIGN KEY (conversation_id, message_id) REFERENCES
    //   messages (conversation_id, id) ON DELETE CASCADE` alongside the
    //   existing single-column FKs (the single-column conversation FK still
    //   guards draft rows, whose `message_id IS NULL` skips the composite
    //   FK by SQLite NULL semantics). Pre-existing cross-conversation rows
    //   (legal before this migration) are repaired to the draft state
    //   (`message_id = NULL`): the file row survives under its conversation
    //   instead of being destroyed or bricking the upgrade.
    // - `agent_tasks` gains `CHECK (current_step <= total_steps)`.
    //   Pre-existing over-advanced rows are clamped to `total_steps`.
    // - `agent_runs` gains `CHECK (status = 'running' OR finished_at IS NOT
    //   NULL)` (a terminal status requires a finish time) and
    //   `CHECK (finished_at IS NULL OR finished_at >= started_at)`.
    //   Pre-existing terminal rows with `finished_at IS NULL` are stamped
    //   with `started_at` (a zero-duration run, the honest minimal repair),
    //   and any `finished_at < started_at` is clamped up to `started_at`.
    //   `total_steps` is normalized to the child-row count, matching the
    //   `finalize_run` recompute discipline (D12: the counter counts the
    //   recorded steps).
    (
        13,
        r"CREATE TABLE messages_new (
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
    created_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (created_at > 0),
    UNIQUE (conversation_id, id)
);

INSERT INTO messages_new
    (id, conversation_id, role, content, provider_id, model_name, created_at)
    SELECT id, conversation_id, role, content, provider_id, model_name, created_at
    FROM messages;

DROP TABLE messages;
ALTER TABLE messages_new RENAME TO messages;

CREATE INDEX idx_messages_conversation_created
    ON messages (conversation_id, created_at);

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

CREATE TABLE attachments_new (
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
    mime_type TEXT CHECK (length(mime_type) <= 127),
    FOREIGN KEY (conversation_id, message_id)
        REFERENCES messages (conversation_id, id) ON DELETE CASCADE
);

INSERT INTO attachments_new
    (id, conversation_id, message_id, file_name, file_path, file_size_bytes, mime_type)
    SELECT a.id, a.conversation_id,
           CASE WHEN m.id IS NULL THEN NULL
                WHEN m.conversation_id != a.conversation_id THEN NULL
                ELSE a.message_id END,
           a.file_name, a.file_path, a.file_size_bytes, a.mime_type
    FROM attachments a LEFT JOIN messages m ON m.id = a.message_id;

DROP TABLE attachments;
ALTER TABLE attachments_new RENAME TO attachments;

CREATE INDEX idx_attachments_conversation
    ON attachments (conversation_id);

CREATE INDEX idx_attachments_message
    ON attachments (message_id);

CREATE INDEX idx_attachments_conversation_message
    ON attachments (conversation_id, message_id);

CREATE TABLE agent_tasks_new (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    title TEXT NOT NULL CHECK (length(title) > 0 AND length(title) <= 200),
    description TEXT
        CHECK (description IS NULL OR length(description) <= 4000),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled')),
    conversation_id INTEGER
        CHECK (conversation_id IS NULL OR conversation_id > 0)
        REFERENCES conversations (id) ON DELETE CASCADE,
    provider TEXT
        CHECK (provider IS NULL OR (length(provider) > 0 AND length(provider) <= 100)),
    model TEXT
        CHECK (model IS NULL OR (length(model) > 0 AND length(model) <= 200)),
    max_steps INTEGER NOT NULL DEFAULT 25 CHECK (max_steps >= 1 AND max_steps <= 25),
    current_step INTEGER NOT NULL DEFAULT 0 CHECK (current_step >= 0),
    total_steps INTEGER NOT NULL DEFAULT 0 CHECK (total_steps >= 0),
    report TEXT,
    run_id INTEGER
        CHECK (run_id IS NULL OR run_id > 0)
        REFERENCES agent_runs (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (created_at > 0),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        CHECK (updated_at >= created_at),
    CHECK (current_step <= total_steps)
);

INSERT INTO agent_tasks_new
    (id, title, description, status, conversation_id, provider, model,
     max_steps, current_step, total_steps, report, run_id, created_at, updated_at)
    SELECT id, title, description, status, conversation_id, provider, model,
           max_steps, min(current_step, total_steps), total_steps, report, run_id,
           created_at, updated_at
    FROM agent_tasks;

DROP TABLE agent_tasks;
ALTER TABLE agent_tasks_new RENAME TO agent_tasks;

CREATE INDEX idx_agent_tasks_status_updated
    ON agent_tasks (status, updated_at);

CREATE TABLE agent_runs_new (
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
    limit_micro_usd INTEGER CHECK (limit_micro_usd IS NULL OR limit_micro_usd >= 0),
    CHECK (status = 'running' OR finished_at IS NOT NULL),
    CHECK (finished_at IS NULL OR finished_at >= started_at)
);

INSERT INTO agent_runs_new
    (id, conversation_id, model, mode, status, started_at, finished_at,
     total_steps, final_content, error, spent_micro_usd, limit_micro_usd)
    SELECT id, conversation_id, model, mode, status, started_at,
           CASE WHEN finished_at IS NULL AND status != 'running' THEN started_at
                WHEN finished_at IS NOT NULL AND finished_at < started_at THEN started_at
                ELSE finished_at END,
           (SELECT COUNT(*) FROM agent_steps s WHERE s.run_id = agent_runs.id),
           final_content, error, spent_micro_usd, limit_micro_usd
    FROM agent_runs;

DROP TABLE agent_runs;
ALTER TABLE agent_runs_new RENAME TO agent_runs;

CREATE INDEX idx_agent_runs_conversation ON agent_runs (conversation_id);
CREATE INDEX idx_agent_runs_started ON agent_runs (started_at);
",
    ),
    // v14 — per-turn token usage on `agent_steps` (NEX-AGENT-005). Two
    // NULL-able integer columns carrying the provider-reported turn usage
    // (`response.usage` in the runner; `NULL` when the provider omitted it).
    // Plain ALTERs: no FK, UNIQUE, or PK is involved, so no rebuild and no
    // FK toggle is needed. The CHECKs admit NULL (pre-existing rows and
    // usage-less turns) and reject negatives; every write path clamps
    // through `u64 -> i64` saturating at `i64::MAX`, far above real usage.
    (
        14,
        r"ALTER TABLE agent_steps ADD COLUMN input_tokens INTEGER
    CHECK (input_tokens IS NULL OR input_tokens >= 0);
ALTER TABLE agent_steps ADD COLUMN output_tokens INTEGER
    CHECK (output_tokens IS NULL OR output_tokens >= 0);
",
    ),
];

/// Open the `SQLite` database at `path`, apply connection pragmas, and run any
/// pending migrations.
///
/// WAL mode, foreign-key enforcement, and a busy timeout are configured on the
/// returned connection (DATABASE.md §3).
///
/// # Errors
///
/// Returns [`DatabaseError::Sqlite`] when the file cannot be opened or a
/// migration fails, [`DatabaseError::SchemaTooNew`] on a forward-version
/// mismatch, [`DatabaseError::SchemaGap`] when a bookkeeping version is
/// missing, [`DatabaseError::Busy`] when another process holds the migration
/// leadership lock past the 5s budget, and [`DatabaseError::Io`] when the
/// leadership lock file cannot be used.
pub fn open(path: impl AsRef<Path>) -> Result<Connection, DatabaseError> {
    let path = path.as_ref();
    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    // Cross-process migration leadership: one process may migrate at a time.
    // A second process waits up to BUSY_TIMEOUT_MS and then becomes a reader
    // (it re-reads the version inside `migrate`); past the budget it fails
    // honestly with `Busy`. The lock is a sibling file so SQLite's own
    // journal/locking behaviour is untouched.
    let _leadership = MigrationLock::acquire(path)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Apply the connection-level pragmas required by DATABASE.md §3.
///
/// # Errors
///
/// Returns [`DatabaseError::Sqlite`] when a pragma cannot be applied.
pub fn configure(conn: &Connection) -> Result<(), DatabaseError> {
    configure_with_busy_timeout(conn, BUSY_TIMEOUT_MS)
}

/// Apply the production pragmas with an explicit busy budget.
///
/// Exposed so tests can prove the busy-honesty path without sleeping for the
/// full five-second production budget. Callers normally use [`configure`].
///
/// # Errors
///
/// Returns [`DatabaseError::Sqlite`] when a pragma cannot be applied.
pub fn configure_with_busy_timeout(
    conn: &Connection,
    busy_timeout_ms: u64,
) -> Result<(), DatabaseError> {
    conn.execute_batch(&format!(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA busy_timeout={busy_timeout_ms};"
    ))?;
    Ok(())
}

/// Migration-leadership lock file beside the database (never inside it).
fn migration_lock_path(db_path: &Path) -> PathBuf {
    let name = db_path.file_name().map_or_else(
        || "nexora.db".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    db_path.with_file_name(format!("{name}.migrate.lock"))
}

/// Cross-process migration leadership held for the duration of `migrate`.
struct MigrationLock {
    file: File,
}

impl MigrationLock {
    /// Wait up to [`BUSY_TIMEOUT_MS`] for exclusive leadership.
    fn acquire(db_path: &Path) -> Result<Self, DatabaseError> {
        let path = migration_lock_path(db_path);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(DatabaseError::Io)?;
        let deadline = Instant::now() + Duration::from_millis(BUSY_TIMEOUT_MS);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(DatabaseError::Busy);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::Error(err)) => return Err(DatabaseError::Io(err)),
            }
        }
    }
}

impl Drop for MigrationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Create the `schema_version` bookkeeping table (DATABASE.md §7.7) and apply
/// any pending migrations.
///
/// # Errors
///
/// Returns [`DatabaseError::SchemaTooNew`] when the file is newer than this
/// build, [`DatabaseError::SchemaGap`] when the applied set is not the
/// contiguous `1..=MAX`, and [`DatabaseError::Sqlite`] when a migration fails
/// (its transaction rolls back as a unit).
pub fn migrate(conn: &mut Connection) -> Result<(), DatabaseError> {
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

    // NEX-DB-007: the runner applies every known version above `MAX(version)`,
    // so a gap at or below it (a skipped or removed bookkeeping row) would
    // otherwise mis-migrate silently — later migrations would apply on top of
    // a schema that never received the missing one, with no recovery path.
    // Detect the gap explicitly and fail loud/early with a typed error before
    // anything is applied. The applied set must be exactly `1..=current`.
    let applied: Vec<i64> = conn
        .prepare("SELECT version FROM schema_version ORDER BY version")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<i64>, _>>()?;
    for (index, version) in applied.iter().enumerate() {
        let expected = i64::try_from(index).unwrap_or(i64::MAX) + 1;
        if *version != expected {
            return Err(DatabaseError::SchemaGap { missing: expected });
        }
    }

    for &(version, sql) in MIGRATIONS {
        if version <= current {
            continue;
        }
        if version == 5 || version == 7 || version == 13 {
            // v5 rebuilds agent_runs, a parent of agent_steps; v7 rebuilds
            // agent_steps itself (adding provenance columns with a new FK to
            // permission_rules); v13 rebuilds messages (parent of the
            // attachment FKs), attachments, agent_tasks (parent of
            // agent_task_steps and task_locks), and agent_runs (parent of
            // agent_steps and the task run links). SQLite cannot hold the FK
            // graph while dropping/recreating, and PRAGMA foreign_keys cannot
            // be toggled inside the transaction that apply_migration opens.
            // The toggle is therefore applied here, outside the tx.
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
///
/// # Errors
///
/// Returns [`DatabaseError::Sqlite`] when the SQL or the bookkeeping insert
/// fails; the transaction is rolled back.
pub fn apply_migration(
    conn: &mut Connection,
    version: i64,
    sql: &str,
) -> Result<(), DatabaseError> {
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
#[must_use]
pub fn target_version() -> i64 {
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

/// Current time as Unix seconds, matching the `unixepoch()` the schema
/// defaults and touch triggers use (test clock-shift scenarios only).
#[must_use]
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Shared application-wide `SQLite` connection.
///
/// Opened exactly once during startup via [`open`] and registered as Tauri
/// managed state, so the single connection outlives `setup` and remains
/// available for the application's entire lifetime (ROADMAP.md Phase 0). Later
/// application components obtain it through `app.state::<Database>()`.
///
/// `rusqlite::Connection` is `Send` but not `Sync`; wrapping it in a `Mutex`
/// satisfies Tauri's `Send + Sync` requirement for managed state. The
/// connection is additionally held behind an [`Arc`] so the handle is
/// cheaply cloneable: the Task 5.1 agent-run bridge moves an owned clone
/// into the spawned run thread, which records steps through the same shared
/// connection (every access still serializes on the one `Mutex`).
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Wrap an already-opened, migrated connection in shared application state.
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }

    /// Acquire the shared connection.
    ///
    /// Returns a guard providing exclusive access to the single connection.
    /// Holders must never write secrets (API keys, tokens, prompts, user
    /// messages) through the connection (ARCHITECTURE.md §12; DATABASE.md §14).
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError::Lock`] when a previous holder poisoned the
    /// lock; the poison is cleared and any open transaction rolled back, so
    /// only the observing call fails.
    ///
    /// NEX-SEC-006: a poisoned mutex (a previous holder panicked mid-access)
    /// is handled gracefully instead of propagating the panic. The poison is
    /// cleared and any transaction the panicking holder left open is rolled
    /// back best-effort, so the connection returns to autocommit for later
    /// holders; the observing call still fails with the typed
    /// [`DatabaseError::Lock`] error (fixed text, never the panic payload),
    /// so one panicking closure costs exactly one failed operation instead of
    /// a process-wide outage.
    pub fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, DatabaseError> {
        match self.conn.lock() {
            Ok(guard) => Ok(guard),
            Err(poisoned) => {
                // Clear the poison (holding the guard alone does not reset
                // it) and reset any transaction the panicking holder left
                // open, so later holders proceed on a clean connection.
                let guard = poisoned.into_inner();
                let _ = guard.execute_batch("ROLLBACK;");
                drop(guard);
                self.conn.clear_poison();
                Err(DatabaseError::Lock(
                    "the database connection was interrupted; retry the operation".to_string(),
                ))
            }
        }
    }
}

/// Open an in-memory database with production pragmas and the full migration
/// set applied. Convenience seam for tests and shells that exercise
/// persistence against the documented schema without a file.
///
/// # Panics
///
/// Panics when the in-memory connection cannot be opened, configured, or
/// migrated — a bug in the pinned migration set, never a runtime condition.
#[must_use]
pub fn in_memory_database() -> Database {
    let mut conn = Connection::open_in_memory().expect("open in-memory database");
    configure(&conn).expect("configure in-memory connection");
    migrate(&mut conn).expect("apply migrations");
    Database::new(conn)
}

#[cfg(test)]
mod core_storage_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("nexora-core-db-{name}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            let path = dir.join("nexora.db");
            Self { dir, path }
        }

        fn config(&self) -> Connection {
            let conn = Connection::open(&self.path).expect("open fixture");
            configure(&conn).expect("configure fixture");
            conn
        }

        fn migrated(&self) -> Connection {
            let mut conn = self.config();
            migrate(&mut conn).expect("migrate fixture");
            conn
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn version_rows(conn: &Connection) -> Vec<i64> {
        conn.prepare("SELECT version FROM schema_version ORDER BY version")
            .expect("prepare versions")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("query versions")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect versions")
    }

    fn schema_snapshot(conn: &Connection) -> Vec<(String, String, String, String)> {
        conn.prepare(
            "SELECT type, name, tbl_name, COALESCE(sql, '') FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .expect("prepare schema")
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("query schema")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect schema")
    }

    #[test]
    fn upgrade_from_v13_fixture_matches_fresh_schema() {
        let target = target_version();
        let upgraded = TempDb::new("upgraded");
        {
            let mut conn = upgraded.config();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (\
                     version INTEGER PRIMARY KEY CHECK (version > 0),\
                     applied_at INTEGER NOT NULL CHECK (applied_at > 0)\
                 );",
            )
            .expect("bookkeeping");
            for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v < target) {
                apply_migration(&mut conn, version, sql).expect("apply fixture migration");
            }
        }
        let mut upgraded_conn = Connection::open(&upgraded.path).expect("reopen fixture");
        configure(&upgraded_conn).expect("configure upgraded");
        migrate(&mut upgraded_conn).expect("finish migrations");

        let fresh = TempDb::new("fresh");
        let fresh_conn = fresh.migrated();

        assert_eq!(
            version_rows(&upgraded_conn),
            (1..=target).collect::<Vec<_>>(),
            "one contiguous bookkeeping row per migration"
        );
        assert_eq!(
            schema_snapshot(&upgraded_conn),
            schema_snapshot(&fresh_conn),
            "an upgraded database must have the same schema a fresh one gets"
        );
    }

    #[test]
    fn downgrade_guard_is_honest_and_non_destructive() {
        let temp = TempDb::new("downgrade");
        let target = target_version();
        {
            let conn = temp.migrated();
            conn.execute(
                "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
                params![target + 1, now_millis()],
            )
            .expect("simulate newer app");
        }
        let before_rows = {
            let conn = temp.config();
            version_rows(&conn)
        };
        let before_size = std::fs::metadata(&temp.path).expect("metadata").len();

        let err = open(&temp.path).expect_err("a newer schema must be refused");
        let expected = format!(
            "database schema version {} is newer than the application's known migrations ({target})",
            target + 1
        );
        assert_eq!(err.to_string(), expected);
        assert!(matches!(
            err,
            DatabaseError::SchemaTooNew {
                db_version,
                app_version
            } if db_version == target + 1 && app_version == target
        ));

        let after_size = std::fs::metadata(&temp.path).expect("metadata").len();
        let conn = temp.config();
        assert_eq!(version_rows(&conn), before_rows, "version rows untouched");
        assert_eq!(after_size, before_size, "file size untouched");
    }

    #[test]
    fn schema_gap_is_honest_verbatim() {
        let temp = TempDb::new("gap");
        {
            let mut conn = temp.config();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (\
                     version INTEGER PRIMARY KEY CHECK (version > 0),\
                     applied_at INTEGER NOT NULL CHECK (applied_at > 0)\
                 );",
            )
            .expect("bookkeeping");
            for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v == 1 || *v == 3) {
                apply_migration(&mut conn, version, sql).expect("apply fixture migration");
            }
        }
        let before_rows = {
            let conn = temp.config();
            version_rows(&conn)
        };
        let before_size = std::fs::metadata(&temp.path).expect("metadata").len();

        let err = open(&temp.path).expect_err("a gap must be refused");
        assert_eq!(
            err.to_string(),
            "database schema is missing migration version 2: the migration history \
             is incomplete and startup cannot continue"
        );
        assert!(matches!(err, DatabaseError::SchemaGap { missing: 2 }));

        let after_size = std::fs::metadata(&temp.path).expect("metadata").len();
        let conn = temp.config();
        assert_eq!(version_rows(&conn), before_rows, "no row was invented");
        assert_eq!(after_size, before_size, "file size untouched");
    }

    #[test]
    fn concurrent_opens_have_exactly_one_migrator() {
        let temp = TempDb::new("concurrent");
        let target = target_version();
        {
            let mut conn = temp.config();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (\
                     version INTEGER PRIMARY KEY CHECK (version > 0),\
                     applied_at INTEGER NOT NULL CHECK (applied_at > 0)\
                 );",
            )
            .expect("bookkeeping");
            let (version, sql) = MIGRATIONS.first().expect("at least one migration");
            apply_migration(&mut conn, *version, sql).expect("fixture v1");
        }

        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let path = temp.path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                open(&path)
            }));
        }
        let mut conns = Vec::new();
        for handle in handles {
            conns.push(handle.join().expect("thread").expect("all opens succeed"));
        }
        for conn in &conns {
            assert_eq!(version_rows(conn), (1..=target).collect::<Vec<_>>());
        }
        assert_eq!(conns.len(), 4);
    }

    #[test]
    fn busy_past_budget_is_honest() {
        let temp = TempDb::new("busy");
        let writer = temp.migrated();
        writer
            .execute_batch("BEGIN IMMEDIATE;")
            .expect("begin writer transaction");
        writer
            .execute(
                "INSERT INTO app_settings (key, value) VALUES ('probe', 'held')",
                [],
            )
            .expect("writer holds the write lock");

        let contender = Connection::open(&temp.path).expect("second connection");
        configure_with_busy_timeout(&contender, 100).expect("short budget");
        let raw = contender
            .execute(
                "INSERT INTO app_settings (key, value) VALUES ('probe2', 'blocked')",
                [],
            )
            .expect_err("writer must block the second writer");
        let err = DatabaseError::from(raw);
        assert!(matches!(err, DatabaseError::Busy), "got {err:?}");
        assert_eq!(
            err.to_string(),
            "database is busy: another Nexora process held the write lock longer than 5s; \
             retry when it finishes"
        );
        writer.execute_batch("ROLLBACK;").expect("rollback");
    }

    #[test]
    fn in_memory_seam_applies_the_whole_migration_set() {
        let db = in_memory_database();
        let conn = db.lock().expect("lock");
        assert_eq!(
            version_rows(&conn),
            (1..=target_version()).collect::<Vec<_>>()
        );
    }
}
