//! `SQLite` foundation: desktop adapter over the shared core storage module.
//!
//! The implementation lives in `nexora_core::storage::database` (issue #161):
//! one pragma/migration bootstrap, one forward-only migration set, one
//! poison-safe shared connection. This module keeps the original paths and
//! names so the desktop's application code and tests did not need to move.
//!
//! Desktop-specific tests (schema, FTS, triggers, repository integration)
//! stay here because they exercise desktop repositories that are not part of
//! the core extraction.

pub(crate) use nexora_core::storage::database::*;

// Test-only imports: the desktop schema/repository tests build raw
// connections and bound parameters directly.
#[cfg(test)]
use rusqlite::{params, Connection};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::search::LocalSearchService;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;
    use crate::infrastructure::repository::agent_tasks::AgentTaskRepository;
    use crate::infrastructure::repository::attachments::AttachmentRepository;
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
            "agent_task_steps",
            "agent_tasks",
            "app_settings",
            "attachments",
            "conversations",
            "debt_items",
            "messages",
            "permission_rules",
            "prompts",
            "providers",
            "task_locks",
            "usage_ledger",
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
            "idx_agent_task_steps_task_seq",
            "idx_agent_tasks_status_updated",
            "idx_attachments_conversation",
            "idx_attachments_conversation_message",
            "idx_attachments_message",
            "idx_conversations_status_updated",
            "idx_debt_items_status_updated",
            "idx_messages_conversation_created",
            "idx_providers_name",
            "idx_task_locks_expires",
            "idx_usage_ledger_day",
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

        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
    }

    #[test]
    fn migration_state_is_recorded_correctly() {
        let conn = in_memory_migrated();

        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );

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
        assert_eq!(version_count, 14, "one row per applied migration");
    }

    #[test]
    fn migration_versions_are_unique_and_strictly_increasing() {
        let mut seen = std::collections::HashSet::new();
        let mut previous = 0;
        for (version, _) in MIGRATIONS {
            assert!(
                seen.insert(*version),
                "duplicate migration version {version}"
            );
            assert!(
                *version > previous,
                "migration versions must increase: {version} after {previous}"
            );
            previous = *version;
        }
    }

    #[test]
    fn re_running_migrations_is_a_no_op() {
        let mut conn = in_memory_migrated();
        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );

        migrate(&mut conn).expect("a second migration run must succeed");

        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
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
    fn migration_gap_fails_loud_with_a_typed_error() {
        // NEX-DB-007: the runner used to key on `MAX(version)`, so a skipped
        // bookkeeping row (here v6) mis-migrated silently — later migrations
        // applied on top of a schema that never received the missing one.
        // Build a v7-shaped history with v6 missing, then migrate: the runner
        // must refuse with `SchemaGap` before applying anything further.
        let mut conn = Connection::open_in_memory().expect("open in-memory");
        configure(&conn).expect("configure");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY CHECK (version > 0), applied_at INTEGER NOT NULL CHECK (applied_at > 0));",
        )
        .expect("create schema_version");
        for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v <= 5) {
            if version == 5 {
                conn.execute_batch("PRAGMA foreign_keys=OFF;")
                    .expect("fk off");
                let result = apply_migration(&mut conn, version, sql);
                conn.execute_batch("PRAGMA foreign_keys=ON;")
                    .expect("fk on");
                result.expect("apply v5");
            } else {
                apply_migration(&mut conn, version, sql).expect("apply v1..v4");
            }
        }
        // Skip v6, apply v7 (the rebuild helper, like the v7 test).
        let (_, v7_sql) = MIGRATIONS
            .iter()
            .find(|(version, _)| *version == 7)
            .expect("v7 exists");
        conn.execute_batch("PRAGMA foreign_keys=OFF;")
            .expect("fk off");
        let v7 = apply_migration(&mut conn, 7, v7_sql);
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .expect("fk on");
        v7.expect("apply v7");
        assert_eq!(schema_version_rows(&conn), vec![1, 2, 3, 4, 5, 7]);

        let err = migrate(&mut conn).expect_err("a gapped history must fail");
        assert!(
            matches!(err, DatabaseError::SchemaGap { missing: 6 }),
            "expected SchemaGap {{ missing: 6 }}, got {err:?}"
        );
        let rendered = format!("{err}");
        assert!(
            rendered.contains('6'),
            "the gap error names the missing version, got {rendered:?}"
        );
        // Loud and early: nothing further was applied.
        assert_eq!(schema_version_rows(&conn), vec![1, 2, 3, 4, 5, 7]);
        assert!(
            !schema_object_exists(&conn, "usage_ledger", "table"),
            "no later migration may apply past a gap"
        );
    }

    #[test]
    fn poisoned_connection_lock_fails_typed_once_then_recovers() {
        // NEX-SEC-006: a panicking holder must cost exactly one failed
        // operation (a typed `Lock` error, never a propagated panic and never
        // the panic payload), and later holders must proceed — no
        // process-wide outage.
        let db = in_memory_database();
        let poisoned = db.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.lock().expect("lock for poisoning");
            panic!("sentinel-poison-payload");
        });
        let err = db.lock().expect_err("the observing call must fail");
        assert!(
            matches!(err, DatabaseError::Lock(_)),
            "expected a typed Lock error, got {err:?}"
        );
        let rendered = format!("{err}");
        assert!(
            !rendered.contains("sentinel-poison-payload"),
            "the lock error must stay fixed-vocabulary, got {rendered:?}"
        );
        // Recovery: the next acquisition succeeds on a usable connection.
        let recovered: i64 = db
            .lock()
            .expect("recovered lock")
            .query_row("SELECT 1", [], |row| row.get(0))
            .expect("connection usable after poison");
        assert_eq!(recovered, 1);
    }

    #[test]
    fn v12_touch_triggers_write_monotonically() {
        // NEX-DB-001/003: the v12 trigger bodies must stamp
        // `max(unixepoch(), created_at)` so a row with a future `created_at`
        // (within-skew import, backward clock move) survives its next update.
        let conn = in_memory_migrated();
        for trigger in ["conversations_touch_updated_at", "prompts_touch_updated_at"] {
            let body: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE name = ?1 AND type = 'trigger'",
                    params![trigger],
                    |row| row.get(0),
                )
                .expect("read trigger body");
            assert!(
                body.contains("max((unixepoch()), created_at)"),
                "{trigger} must write monotonically, found: {body}"
            );
        }
    }

    #[test]
    fn rename_after_created_at_moves_to_the_future_still_succeeds() {
        // NEX-DB-003 acceptance for `conversations`: simulate a backward
        // wall-clock movement (`created_at` pushed ahead of now), then rename
        // through the repository. Before the fix the trigger's plain
        // `unixepoch()` stamp violated the CHECK; now the write is monotonic.
        let db = in_memory_database();
        let repo = ConversationRepository::new(&db);
        let id = repo.create("t", "active").expect("create conversation");
        let future = now_secs() + 100;
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "UPDATE conversations SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
                params![future, id],
            )
            .expect("push created_at into the future");
        }
        repo.update(id, "renamed", "active")
            .expect("rename with future created_at succeeds");
        let row = repo.read(id).expect("read").expect("exists");
        assert_eq!(row.title, "renamed");
        assert!(
            row.updated_at >= row.created_at,
            "updated_at must stay monotonic: {row:?}"
        );
    }

    #[test]
    fn prompt_edit_after_created_at_moves_to_the_future_still_succeeds() {
        // NEX-DB-003 acceptance for `prompts`: same backward-clock scenario
        // through the prompts trigger.
        let db = in_memory_database();
        let repo = PromptRepository::new(&db);
        let id = repo.create("t", "body").expect("create prompt");
        let future = now_secs() + 100;
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "UPDATE prompts SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
                params![future, id],
            )
            .expect("push created_at into the future");
        }
        repo.update(id, "renamed", "new body")
            .expect("prompt edit with future created_at succeeds");
        let row = repo.read(id).expect("read").expect("exists");
        assert!(
            row.updated_at >= row.created_at,
            "updated_at must stay monotonic: {row:?}"
        );
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
        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
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
        // Schema version covers every known migration (v5 plus later ones)
        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
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
        // New status is now accepted (a terminal insert must stamp
        // finished_at per the v13 lifecycle CHECK).
        conn.execute(
            "INSERT INTO agent_runs (model, mode, status, finished_at) \
             VALUES ('m', 'supervised', 'spend_limit_exceeded', (unixepoch()))",
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

    #[test]
    fn v5_to_v6_preserves_rows_and_adds_nullable_workspace_root() {
        // Build a v5-only DB: apply migrations 1..=5 manually, seed rows, then
        // migrate to v6.
        let mut conn = Connection::open_in_memory().expect("open in-memory");
        configure(&conn).expect("configure");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY CHECK (version > 0), applied_at INTEGER NOT NULL CHECK (applied_at > 0));",
        )
        .expect("create schema_version");
        for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v <= 5) {
            if version == 5 {
                conn.execute_batch("PRAGMA foreign_keys=OFF;")
                    .expect("fk off");
                let result = apply_migration(&mut conn, version, sql);
                conn.execute_batch("PRAGMA foreign_keys=ON;")
                    .expect("fk on");
                result.expect("apply v5");
            } else {
                apply_migration(&mut conn, version, sql).expect("apply v1..v5");
            }
        }
        conn.execute("INSERT INTO conversations (title) VALUES ('keep me')", [])
            .expect("seed conversation");
        let conv_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO messages (conversation_id, role, content) VALUES (?1, 'user', 'hi')",
            [conv_id],
        )
        .expect("seed message");
        migrate(&mut conn).expect("migrate to v6");
        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
        // Seeded row survives with a NULL workspace root.
        let root: Option<String> = conn
            .query_row(
                "SELECT workspace_root FROM conversations WHERE id = ?1",
                [conv_id],
                |row| row.get(0),
            )
            .expect("read preserved conversation");
        assert_eq!(root, None);
        let messages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE conversation_id = ?1",
                [conv_id],
                |row| row.get(0),
            )
            .expect("count messages");
        assert_eq!(messages, 1);
        // New column accepts a root and rejects an overlong one.
        conn.execute(
            "UPDATE conversations SET workspace_root = ?1 WHERE id = ?2",
            rusqlite::params![r"C:\work\proj", conv_id],
        )
        .expect("workspace root accepted");
        let long_root = "r".repeat(1025);
        let overlong = conn.execute(
            "UPDATE conversations SET workspace_root = ?1 WHERE id = ?2",
            rusqlite::params![long_root, conv_id],
        );
        assert!(
            overlong.is_err(),
            "overlong workspace root must be rejected"
        );
    }

    /// Build a v6-only in-memory database with one completed run and one
    /// `model_turn` step; returns the connection and the run id.
    fn v6_database_with_run_and_step() -> (Connection, i64) {
        let mut conn = Connection::open_in_memory().expect("open in-memory");
        configure(&conn).expect("configure");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY CHECK (version > 0), applied_at INTEGER NOT NULL CHECK (applied_at > 0));",
        )
        .expect("create schema_version");
        for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v <= 6) {
            if version == 5 {
                conn.execute_batch("PRAGMA foreign_keys=OFF;")
                    .expect("fk off");
                let result = apply_migration(&mut conn, version, sql);
                conn.execute_batch("PRAGMA foreign_keys=ON;")
                    .expect("fk on");
                result.expect("apply v5");
            } else {
                apply_migration(&mut conn, version, sql).expect("apply v1..v6");
            }
        }
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
        (conn, run_id)
    }

    #[test]
    fn v7_adds_permission_rules_and_step_provenance_preserving_rows() {
        // Build a v6-only DB (mirrors the v5 test), then migrate to v7.
        let (mut conn, run_id) = v6_database_with_run_and_step();
        migrate(&mut conn).expect("migrate to v7");
        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
        // Old rows intact with NULL provenance.
        let (kind, rule_id, group_key, decided_by): (
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT kind, rule_id, group_key, decided_by FROM agent_steps WHERE run_id = ?1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read preserved step");
        assert_eq!(kind, "model_turn");
        assert_eq!(rule_id, None, "pre-v7 rule_id is NULL");
        assert_eq!(group_key, None, "pre-v7 group_key is NULL");
        assert_eq!(decided_by, None, "pre-v7 decided_by is NULL");
        // Seed present.
        let seed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM permission_rules WHERE preset = 'coding' \
                 AND tool_pattern = 'execute_command' AND path_pattern = '*' \
                 AND effect = 'ask' AND priority = 50",
                [],
                |row| row.get(0),
            )
            .expect("count seed");
        assert_eq!(seed, 1, "v7 seeds the coding shell-ask row");
        // UNIQUE enforced.
        let dup = conn.execute(
            "INSERT INTO permission_rules (preset, tool_pattern, path_pattern, effect) \
             VALUES ('coding', 'execute_command', '*', 'ask')",
            [],
        );
        assert!(
            dup.is_err(),
            "UNIQUE(preset, tool_pattern, path_pattern) must reject"
        );
        // CHECKs enforced.
        assert!(
            conn.execute(
                "INSERT INTO permission_rules (preset, tool_pattern, effect) \
                 VALUES ('nope', 'read_file', 'allow')",
                [],
            )
            .is_err(),
            "bad preset must be rejected"
        );
        assert!(
            conn.execute(
                "INSERT INTO permission_rules (preset, tool_pattern, effect) \
                 VALUES ('coding', '', 'allow')",
                [],
            )
            .is_err(),
            "empty tool_pattern must be rejected"
        );
        assert!(
            conn.execute(
                "INSERT INTO permission_rules (preset, tool_pattern, effect) \
                 VALUES ('coding', 'read_file', 'maybe')",
                [],
            )
            .is_err(),
            "bad effect must be rejected"
        );
        assert!(
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind, decided_by) \
                 VALUES (?1, 2, 'model_turn', 'nobody')",
                [run_id],
            )
            .is_err(),
            "bad decided_by must be rejected"
        );
        // Index recreated.
        assert!(
            schema_object_exists(&conn, "idx_agent_steps_run_seq", "index"),
            "idx_agent_steps_run_seq must survive the rebuild"
        );
        // FK still ON after rebuild.
        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("fk pragma");
        assert_eq!(fk_on, 1, "FK must be ON after v7");
    }

    /// Build a v12-only in-memory database (all migrations through the
    /// monotonic touch triggers), mirroring the v5/v6 test pattern: the v5
    /// and v7 rebuilds need the same `foreign_keys` toggle `migrate` applies.
    fn v12_database() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory");
        configure(&conn).expect("configure");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY CHECK (version > 0), applied_at INTEGER NOT NULL CHECK (applied_at > 0));",
        )
        .expect("create schema_version");
        for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v <= 12) {
            if version == 5 || version == 7 {
                conn.execute_batch("PRAGMA foreign_keys=OFF;")
                    .expect("fk off");
                let result = apply_migration(&mut conn, version, sql);
                conn.execute_batch("PRAGMA foreign_keys=ON;")
                    .expect("fk on");
                result.expect("apply rebuild migration");
            } else {
                apply_migration(&mut conn, version, sql).expect("apply migration");
            }
        }
        conn
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn v13_preserves_legal_rows_repairs_impossible_ones_and_enforces_invariants() {
        // NEX-DB-002/004/006 acceptance: pre-existing rows (including edge
        // rows that stay legal, plus rows that were legal on v12 but violate
        // the new cross-column invariants) survive the v13 rebuilds; the
        // illegal ones are repaired, and the new invariants hold afterwards.
        let conn = v12_database();
        let db = Database::new(conn);
        let conversations = ConversationRepository::new(&db);
        let messages = MessageRepository::new(&db);
        let attachments = AttachmentRepository::new(&db);
        let tasks = AgentTaskRepository::new(&db);
        let runs = AgentRunRepository::new(&db);

        let conv_a = conversations.create("a", "active").expect("conv a");
        let conv_b = conversations.create("b", "active").expect("conv b");
        let msg_a = messages
            .create(conv_a, "user", "v13 searchable alpha", None, None)
            .expect("msg a");
        let msg_b = messages
            .create(conv_b, "user", "v13 searchable beta", None, None)
            .expect("msg b");

        // Legal edge rows: a linked attachment, a draft attachment, and a
        // cross-conversation link (legal on v12, repaired to draft by v13).
        let linked = attachments
            .create(conv_a, "linked.txt", "/tmp/linked.txt", None, None)
            .expect("linked attachment");
        attachments
            .update_message_id(linked, Some(msg_a))
            .expect("link");
        let draft = attachments
            .create(conv_a, "draft.txt", "/tmp/draft.txt", None, None)
            .expect("draft attachment");
        let crossed = attachments
            .create(conv_a, "crossed.txt", "/tmp/crossed.txt", None, None)
            .expect("cross attachment");
        attachments
            .update_message_id(crossed, Some(msg_b))
            .expect("v12 allows the cross-conversation link");

        // Legal task plus an over-advanced one (legal on v12, clamped by v13).
        let task_ok = tasks
            .create_task("ok", None, Some(conv_a), None, None, 25)
            .expect("task");
        tasks.mark_task_running(task_ok, 3).expect("running");
        tasks
            .mark_task_progress(task_ok, 2, None)
            .expect("progress");
        let task_over = tasks
            .create_task("over", None, None, None, None, 25)
            .expect("task");
        tasks.mark_task_running(task_over, 2).expect("running");
        tasks
            .mark_task_progress(task_over, 999, None)
            .expect("v12 allows progress past the plan");

        // Legal runs: a finalized run with two steps, a still-running run,
        // a terminal row with `finished_at IS NULL` (legal on v12), and a
        // terminal row with `finished_at < started_at` (legal on v12).
        let run_ok = runs
            .create_run(Some(conv_a), "m", "supervised")
            .expect("run");
        for seq in 1..=2 {
            // v12-era writes carry no usage columns: seed with raw SQL
            // (mirroring the v5/v7 tests) — the repo's `append_step`
            // targets the current (v14) shape.
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind) VALUES (?1, ?2, 'model_turn')",
                params![run_ok, seq],
            )
            .expect("step");
        }
        runs.finalize_run(run_ok, "completed", Some("done"), None, None, None)
            .expect("finalize");
        let run_active = runs.create_run(None, "m", "supervised").expect("run");
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind) VALUES (?1, 1, 'model_turn')",
                [run_active],
            )
            .expect("step");
        }
        let run_null: i64 = {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "INSERT INTO agent_runs (model, mode, status) VALUES ('m', 'supervised', 'completed')",
                [],
            )
            .expect("v12 allows terminal status without finished_at");
            conn.last_insert_rowid()
        };
        let run_back = runs.create_run(None, "m", "supervised").expect("run");
        runs.finalize_run(run_back, "cancelled", None, None, None, None)
            .expect("finalize");
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "UPDATE agent_runs SET finished_at = started_at - 1 WHERE id = ?1",
                [run_back],
            )
            .expect("v12 allows finished_at < started_at");
        }

        // Migrate to v13 (and record the version).
        {
            let mut guard = db.lock().expect("lock connection");
            migrate(&mut guard).expect("migrate to v13");
        }
        let conn = db.lock().expect("lock connection");
        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("fk pragma");
        assert_eq!(fk_on, 1, "FK must be ON after v13");
        drop(conn);

        // Legal rows survive with their values intact.
        let row = attachments.read(linked).expect("read").expect("exists");
        assert_eq!(row.message_id, Some(msg_a), "legal link preserved");
        let row = attachments.read(draft).expect("read").expect("exists");
        assert_eq!(row.message_id, None, "draft preserved");
        let row = attachments.read(crossed).expect("read").expect("exists");
        assert_eq!(
            row.message_id, None,
            "cross-conversation link repaired to the draft state"
        );
        assert_eq!(row.conversation_id, conv_a, "owning conversation kept");
        assert_eq!(row.file_name, "crossed.txt", "file metadata kept");

        let row = tasks.read_task(task_ok).expect("read").expect("exists");
        assert_eq!((row.current_step, row.total_steps), (2, 3));
        let row = tasks.read_task(task_over).expect("read").expect("exists");
        assert_eq!(
            (row.current_step, row.total_steps),
            (2, 2),
            "over-advanced progress clamped to the plan"
        );

        let row = runs.read_run(run_ok).expect("read").expect("exists");
        assert_eq!(row.status, "completed");
        assert_eq!(row.total_steps, 2, "step count preserved");
        assert!(
            row.finished_at.expect("finished") >= row.started_at,
            "ordering holds: {row:?}"
        );
        let row = runs.read_run(run_active).expect("read").expect("exists");
        assert_eq!(row.status, "running");
        assert_eq!(row.finished_at, None);
        let row = runs.read_run(run_null).expect("read").expect("exists");
        assert_eq!(
            row.finished_at,
            Some(row.started_at),
            "terminal row without finished_at stamped at started_at"
        );
        let row = runs.read_run(run_back).expect("read").expect("exists");
        assert_eq!(
            row.finished_at,
            Some(row.started_at),
            "backward finished_at clamped up to started_at"
        );

        // The messages rebuild keeps FTS in sync (triggers recreated).
        let search = LocalSearchService::new(&db);
        let hits = search.search("alpha").expect("search");
        assert!(
            hits.message_matches
                .iter()
                .any(|m| m.conversation_id == conv_a),
            "rebuilt messages table stays searchable"
        );

        // The new invariants are enforced after the migration.
        let conn = db.lock().expect("lock connection");
        assert!(
            conn.execute(
                "INSERT INTO attachments (conversation_id, message_id, file_name, file_path) \
                 VALUES (?1, ?2, 'x.txt', '/tmp/x.txt')",
                params![conv_a, msg_b],
            )
            .is_err(),
            "cross-conversation attachment insert must be rejected"
        );
        assert!(
            conn.execute(
                "UPDATE agent_tasks SET current_step = 99 WHERE id = ?1",
                [task_ok],
            )
            .is_err(),
            "current_step past total_steps must be rejected"
        );
        assert!(
            conn.execute(
                "UPDATE agent_tasks SET current_step = -1 WHERE id = ?1",
                [task_ok],
            )
            .is_err(),
            "negative current_step must be rejected"
        );
        assert!(
            conn.execute(
                "INSERT INTO agent_runs (model, mode, status) VALUES ('m', 'supervised', 'completed')",
                [],
            )
            .is_err(),
            "terminal status without finished_at must be rejected"
        );
        assert!(
            conn.execute(
                "UPDATE agent_runs SET finished_at = started_at - 1 WHERE id = ?1",
                [run_ok],
            )
            .is_err(),
            "finished_at < started_at must be rejected"
        );
    }

    /// Build a v13-only in-memory database (all migrations through the
    /// cross-column invariants), mirroring `v12_database`: the v5/v7/v13
    /// rebuilds need the same `foreign_keys` toggle `migrate` applies.
    fn v13_database() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory");
        configure(&conn).expect("configure");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY CHECK (version > 0), applied_at INTEGER NOT NULL CHECK (applied_at > 0));",
        )
        .expect("create schema_version");
        for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v <= 13) {
            if version == 5 || version == 7 || version == 13 {
                conn.execute_batch("PRAGMA foreign_keys=OFF;")
                    .expect("fk off");
                let result = apply_migration(&mut conn, version, sql);
                conn.execute_batch("PRAGMA foreign_keys=ON;")
                    .expect("fk on");
                result.expect("apply rebuild migration");
            } else {
                apply_migration(&mut conn, version, sql).expect("apply migration");
            }
        }
        conn
    }

    #[test]
    fn v14_adds_step_token_columns_preserving_rows() {
        // NEX-AGENT-005 acceptance: a usage-less step seeded on v13 keeps
        // its values through the v14 ALTERs and reads back NULL usage
        // (honest unknown); the new columns then accept reported usage and
        // reject negatives.
        let conn = v13_database();
        let db = Database::new(conn);
        let runs = AgentRunRepository::new(&db);
        let run_id = runs.create_run(None, "m", "supervised").expect("run");
        // The pre-v14 row is written the v13-era way (no usage columns);
        // the repo's `append_step` targets the current (v14) shape.
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind) VALUES (?1, 1, 'model_turn')",
                [run_id],
            )
            .expect("pre-v14 step");
        }
        {
            let mut guard = db.lock().expect("lock connection");
            migrate(&mut guard).expect("migrate to v14");
        }
        let conn = db.lock().expect("lock connection");
        assert_eq!(
            schema_version_rows(&conn),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
        // Pre-existing row survives with NULL usage (honest unknown).
        let (input, output): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT input_tokens, output_tokens FROM agent_steps WHERE run_id = ?1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read preserved step");
        assert_eq!(input, None, "pre-v14 input_tokens is NULL");
        assert_eq!(output, None, "pre-v14 output_tokens is NULL");
        // New columns accept reported usage and reject negatives.
        conn.execute(
            "UPDATE agent_steps SET input_tokens = 12, output_tokens = 34 WHERE run_id = ?1",
            [run_id],
        )
        .expect("usage write succeeds");
        assert!(
            conn.execute(
                "UPDATE agent_steps SET input_tokens = -1 WHERE run_id = ?1",
                [run_id],
            )
            .is_err(),
            "negative input_tokens must be rejected"
        );
        assert!(
            conn.execute(
                "UPDATE agent_steps SET output_tokens = -1 WHERE run_id = ?1",
                [run_id],
            )
            .is_err(),
            "negative output_tokens must be rejected"
        );
    }
}
