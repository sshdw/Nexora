//! Disk-backed file checkpoint metadata repository (`file_checkpoints`, v17).
//!
//! Raw pre-mutation bytes live content-addressed under
//! `<app_data>/checkpoints/<sha256>` on disk; `SQLite` stores only digests,
//! sizes, paths, and revert state.

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use crate::storage::database::{now_secs, Database};
use crate::storage::repository::{Repository, Result};

/// Metadata row for a single pre/post-mutation file checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileCheckpoint {
    /// Primary key of the checkpoint row.
    pub id: i64,
    /// Owning conversation identifier.
    pub conversation_id: i64,
    /// Owning agent run identifier.
    pub run_id: i64,
    /// Optional linked `agent_steps.id` row (`NULL` if detached or pre-step).
    pub step_id: Option<i64>,
    /// Zero-based agent loop turn index within the run.
    pub turn_index: i64,
    /// Canonical workspace root string recorded for the tool call.
    pub workspace_root: String,
    /// Workspace-relative POSIX-normalized file path.
    pub relative_path: String,
    /// Mutating tool name (`write_file` or `edit_file`).
    pub tool_name: String,
    /// Whether the target file existed on disk prior to mutation.
    pub pre_existed: bool,
    /// Lowercase SHA-256 hex digest of the pre-mutation blob when `pre_existed` is true.
    pub pre_blob_sha256: Option<String>,
    /// Pre-mutation file byte length when `pre_existed` is true.
    pub pre_size_bytes: Option<i64>,
    /// Lowercase SHA-256 hex digest of the file after tool mutation completed.
    pub post_blob_sha256: Option<String>,
    /// Post-mutation file byte length.
    pub post_size_bytes: Option<i64>,
    /// Unix timestamp (seconds) when this checkpoint was reverted, if ever.
    pub reverted_at: Option<i64>,
    /// Unix timestamp (seconds) when the pre-mutation checkpoint was recorded.
    pub created_at: i64,
}

/// Parameters required to record a pre-mutation checkpoint row.
#[derive(Debug, Clone, Copy)]
pub struct NewFileCheckpoint<'a> {
    /// Owning conversation identifier.
    pub conversation_id: i64,
    /// Owning agent run identifier.
    pub run_id: i64,
    /// Optional linked `agent_steps.id` row.
    pub step_id: Option<i64>,
    /// Zero-based agent loop turn index within the run.
    pub turn_index: i64,
    /// Canonical workspace root string recorded for the tool call.
    pub workspace_root: &'a str,
    /// Workspace-relative POSIX-normalized file path.
    pub relative_path: &'a str,
    /// Mutating tool name (`write_file` or `edit_file`).
    pub tool_name: &'a str,
    /// Whether the target file existed on disk prior to mutation.
    pub pre_existed: bool,
    /// Lowercase SHA-256 hex digest of the pre-mutation blob (`Some` iff `pre_existed`).
    pub pre_blob_sha256: Option<&'a str>,
    /// Pre-mutation file byte length (`Some` when `pre_existed`).
    pub pre_size_bytes: Option<i64>,
}

const SELECT_COLUMNS: &str = "id, conversation_id, run_id, step_id, turn_index, workspace_root, \
    relative_path, tool_name, pre_existed, pre_blob_sha256, pre_size_bytes, \
    post_blob_sha256, post_size_bytes, reverted_at, created_at";

fn map_checkpoint(row: &Row<'_>) -> rusqlite::Result<FileCheckpoint> {
    let pre_existed: i64 = row.get(8)?;
    Ok(FileCheckpoint {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        run_id: row.get(2)?,
        step_id: row.get(3)?,
        turn_index: row.get(4)?,
        workspace_root: row.get(5)?,
        relative_path: row.get(6)?,
        tool_name: row.get(7)?,
        pre_existed: pre_existed != 0,
        pre_blob_sha256: row.get(9)?,
        pre_size_bytes: row.get(10)?,
        post_blob_sha256: row.get(11)?,
        post_size_bytes: row.get(12)?,
        reverted_at: row.get(13)?,
        created_at: row.get(14)?,
    })
}

/// Persistence adapter over the `file_checkpoints` table.
pub struct FileCheckpointRepository<'a> {
    db: &'a Database,
}

impl<'a> FileCheckpointRepository<'a> {
    /// Creates a new repository handle bound to `db`.
    #[must_use]
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Inserts a pre-mutation checkpoint row and returns its primary key.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] if the connection
    /// lock fails or any `file_checkpoints` FK / CHECK constraint rejects the
    /// row.
    pub fn insert_pre_mutation(&self, entry: &NewFileCheckpoint<'_>) -> Result<i64> {
        let conn = self.conn()?;
        let now = now_secs();
        conn.execute(
            "INSERT INTO file_checkpoints (
                conversation_id, run_id, step_id, turn_index, workspace_root,
                relative_path, tool_name, pre_existed, pre_blob_sha256,
                pre_size_bytes, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.conversation_id,
                entry.run_id,
                entry.step_id,
                entry.turn_index,
                entry.workspace_root,
                entry.relative_path,
                entry.tool_name,
                i64::from(entry.pre_existed),
                entry.pre_blob_sha256,
                entry.pre_size_bytes,
                now,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Records the post-mutation SHA-256 digest and byte size after disk write.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] on lock or SQL
    /// constraint failure.
    pub fn record_post_mutation(
        &self,
        id: i64,
        post_blob_sha256: &str,
        post_size_bytes: i64,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE file_checkpoints
             SET post_blob_sha256 = ?1, post_size_bytes = ?2
             WHERE id = ?3",
            params![post_blob_sha256, post_size_bytes, id],
        )?;
        Ok(())
    }

    /// Links a checkpoint row to the persisted `agent_steps.id` once appended.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] on lock or SQL
    /// failure.
    pub fn link_step(&self, id: i64, step_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE file_checkpoints SET step_id = ?1 WHERE id = ?2",
            params![step_id, id],
        )?;
        Ok(())
    }

    /// Marks a checkpoint as reverted (idempotently preserving the first
    /// timestamp) and returns the updated row if present.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] on lock or SQL
    /// failure.
    pub fn mark_reverted(
        &self,
        id: i64,
        reverted_at: Option<i64>,
    ) -> Result<Option<FileCheckpoint>> {
        let ts = reverted_at.unwrap_or_else(now_secs);
        {
            let conn = self.conn()?;
            conn.execute(
                "UPDATE file_checkpoints
                 SET reverted_at = COALESCE(reverted_at, ?1)
                 WHERE id = ?2",
                params![ts, id],
            )?;
        }
        self.read(id)
    }

    /// Reads one checkpoint row by primary key.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] on lock or query
    /// failure.
    pub fn read(&self, id: i64) -> Result<Option<FileCheckpoint>> {
        let conn = self.read_conn()?;
        let sql = format!("SELECT {SELECT_COLUMNS} FROM file_checkpoints WHERE id = ?1");
        conn.query_row(&sql, [id], map_checkpoint)
            .optional()
            .map_err(Into::into)
    }

    /// Lists all checkpoints recorded for a run in `(turn_index ASC, id ASC)`
    /// order.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] on lock or query
    /// failure.
    pub fn list_by_run(&self, run_id: i64) -> Result<Vec<FileCheckpoint>> {
        let conn = self.read_conn()?;
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM file_checkpoints
             WHERE run_id = ?1
             ORDER BY turn_index ASC, id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([run_id], map_checkpoint)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Lists checkpoints for one turn of a run in reverse chronological order
    /// (`id DESC`) so multi-step edits unwind cleanly.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] on lock or query
    /// failure.
    pub fn list_by_turn_desc(&self, run_id: i64, turn_index: i64) -> Result<Vec<FileCheckpoint>> {
        let conn = self.read_conn()?;
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM file_checkpoints
             WHERE run_id = ?1 AND turn_index = ?2
             ORDER BY id DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![run_id, turn_index], map_checkpoint)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Collects distinct pre-mutation blob digests associated with a
    /// conversation prior to conversation deletion.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] on lock or query
    /// failure.
    pub fn collect_pre_blobs_for_conversation(&self, conversation_id: i64) -> Result<Vec<String>> {
        let conn = self.read_conn()?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pre_blob_sha256
             FROM file_checkpoints
             WHERE conversation_id = ?1 AND pre_blob_sha256 IS NOT NULL",
        )?;
        let rows = stmt.query_map([conversation_id], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Returns whether any remaining checkpoint row still references the given
    /// pre-mutation blob digest.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] on lock or query
    /// failure.
    pub fn is_blob_referenced(&self, sha256: &str) -> Result<bool> {
        let conn = self.read_conn()?;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM file_checkpoints WHERE pre_blob_sha256 = ?1 LIMIT 1",
                [sha256],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Collects all distinct pre-mutation blob digests currently referenced by
    /// any row in `file_checkpoints`.
    ///
    /// # Errors
    /// Returns [`crate::storage::database::DatabaseError`] on lock or query
    /// failure.
    pub fn all_pre_blobs(&self) -> Result<Vec<String>> {
        let conn = self.read_conn()?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pre_blob_sha256
             FROM file_checkpoints
             WHERE pre_blob_sha256 IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

impl Repository for FileCheckpointRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::in_memory_database;
    use crate::storage::repository::agent_runs::AgentRunRepository;

    fn seed_conv_and_run(db: &Database) -> (i64, i64) {
        let conv_id = {
            let conn = db.lock().expect("lock db");
            conn.execute(
                "INSERT INTO conversations (title, status) VALUES ('c', 'active')",
                [],
            )
            .expect("insert conversation");
            conn.last_insert_rowid()
        };
        let run_id = AgentRunRepository::new(db)
            .create_run(Some(conv_id), "test-model", "semi_autonomous")
            .expect("create run");
        (conv_id, run_id)
    }

    #[test]
    fn crud_step_link_revert_and_listing_work() {
        let db = in_memory_database();
        let (conv_id, run_id) = seed_conv_and_run(&db);
        let step_id = AgentRunRepository::new(&db)
            .append_step(
                run_id,
                1,
                "tool_call",
                Some("write_file"),
                None,
                None,
                Some("succeeded"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("append step");
        let repo = FileCheckpointRepository::new(&db);

        let sha_pre = "a".repeat(64);
        let sha_post = "b".repeat(64);
        let id1 = repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_id,
                run_id,
                step_id: None,
                turn_index: 0,
                workspace_root: "/ws",
                relative_path: "src/main.rs",
                tool_name: "write_file",
                pre_existed: true,
                pre_blob_sha256: Some(&sha_pre),
                pre_size_bytes: Some(12),
            })
            .expect("insert existing checkpoint");
        repo.record_post_mutation(id1, &sha_post, 24)
            .expect("record post");
        repo.link_step(id1, step_id).expect("link step");

        let id2 = repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_id,
                run_id,
                step_id: Some(step_id),
                turn_index: 0,
                workspace_root: "/ws",
                relative_path: "src/new.rs",
                tool_name: "edit_file",
                pre_existed: false,
                pre_blob_sha256: None,
                pre_size_bytes: None,
            })
            .expect("insert new-file checkpoint");

        let asc = repo.list_by_run(run_id).expect("list by run");
        assert_eq!(asc.len(), 2);
        assert_eq!(asc[0].id, id1);
        assert_eq!(asc[0].step_id, Some(step_id));
        assert!(asc[0].pre_existed);
        assert_eq!(asc[0].pre_blob_sha256.as_deref(), Some(sha_pre.as_str()));
        assert_eq!(asc[0].post_blob_sha256.as_deref(), Some(sha_post.as_str()));
        assert_eq!(asc[0].post_size_bytes, Some(24));
        assert_eq!(asc[1].id, id2);
        assert!(!asc[1].pre_existed);
        assert_eq!(asc[1].pre_blob_sha256, None);

        let desc = repo.list_by_turn_desc(run_id, 0).expect("list turn desc");
        assert_eq!(
            desc.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![id2, id1]
        );

        let reverted = repo
            .mark_reverted(id1, Some(1_700_000_123))
            .expect("mark reverted")
            .expect("row exists");
        assert_eq!(reverted.reverted_at, Some(1_700_000_123));
        // Second call preserves original reverted_at timestamp.
        let reverted_again = repo
            .mark_reverted(id1, Some(1_700_000_999))
            .expect("idempotent mark reverted")
            .expect("row exists");
        assert_eq!(reverted_again.reverted_at, Some(1_700_000_123));
    }

    #[test]
    fn cascades_on_conversation_delete_and_nulls_step_delete() {
        let db = in_memory_database();
        let (conv_a, run_a) = seed_conv_and_run(&db);
        let (conv_b, run_b) = seed_conv_and_run(&db);
        let step_a = AgentRunRepository::new(&db)
            .append_step(
                run_a,
                1,
                "tool_call",
                Some("write_file"),
                None,
                None,
                Some("succeeded"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("step a");
        let repo = FileCheckpointRepository::new(&db);

        let shared_sha = "c".repeat(64);
        let unique_sha = "d".repeat(64);
        let cp_a = repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_a,
                run_id: run_a,
                step_id: Some(step_a),
                turn_index: 0,
                workspace_root: "/ws",
                relative_path: "a.txt",
                tool_name: "write_file",
                pre_existed: true,
                pre_blob_sha256: Some(&unique_sha),
                pre_size_bytes: Some(5),
            })
            .expect("cp_a");
        let _cp_shared_a = repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_a,
                run_id: run_a,
                step_id: None,
                turn_index: 1,
                workspace_root: "/ws",
                relative_path: "shared.txt",
                tool_name: "edit_file",
                pre_existed: true,
                pre_blob_sha256: Some(&shared_sha),
                pre_size_bytes: Some(5),
            })
            .expect("cp_shared_a");
        let _cp_b = repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_b,
                run_id: run_b,
                step_id: None,
                turn_index: 0,
                workspace_root: "/ws",
                relative_path: "b.txt",
                tool_name: "write_file",
                pre_existed: true,
                pre_blob_sha256: Some(&shared_sha),
                pre_size_bytes: Some(5),
            })
            .expect("cp_b");

        // Deleting agent_steps sets step_id = NULL without deleting the checkpoint.
        {
            let conn = db.lock().expect("lock");
            conn.execute("DELETE FROM agent_steps WHERE id = ?1", [step_a])
                .expect("delete step");
        }
        assert_eq!(
            repo.read(cp_a).expect("read cp_a").expect("exists").step_id,
            None
        );

        let blobs_a = repo
            .collect_pre_blobs_for_conversation(conv_a)
            .expect("collect blobs");
        assert_eq!(blobs_a.len(), 2);

        {
            let conn = db.lock().expect("lock");
            conn.execute("DELETE FROM conversations WHERE id = ?1", [conv_a])
                .expect("delete conv_a");
        }
        assert!(repo.list_by_run(run_a).expect("list run_a").is_empty());
        assert!(!repo.is_blob_referenced(&unique_sha).expect("check unique"));
        assert!(repo.is_blob_referenced(&shared_sha).expect("check shared"));
        assert_eq!(repo.all_pre_blobs().expect("all blobs"), vec![shared_sha]);
    }

    #[test]
    fn migration_check_constraints_reject_invalid_rows() {
        let db = in_memory_database();
        let (conv_id, run_id) = seed_conv_and_run(&db);
        let repo = FileCheckpointRepository::new(&db);

        // pre_existed=1 requires non-null 64-char lowercase hex sha256.
        assert!(repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_id,
                run_id,
                step_id: None,
                turn_index: 0,
                workspace_root: "/ws",
                relative_path: "a.txt",
                tool_name: "write_file",
                pre_existed: true,
                pre_blob_sha256: None,
                pre_size_bytes: Some(1),
            })
            .is_err());

        // pre_existed=0 forbids non-null pre_blob_sha256.
        let valid_sha = "e".repeat(64);
        assert!(repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_id,
                run_id,
                step_id: None,
                turn_index: 0,
                workspace_root: "/ws",
                relative_path: "a.txt",
                tool_name: "write_file",
                pre_existed: false,
                pre_blob_sha256: Some(&valid_sha),
                pre_size_bytes: None,
            })
            .is_err());

        // Uppercase or bad hex rejected.
        let upper_sha = "A".repeat(64);
        assert!(repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_id,
                run_id,
                step_id: None,
                turn_index: 0,
                workspace_root: "/ws",
                relative_path: "a.txt",
                tool_name: "write_file",
                pre_existed: true,
                pre_blob_sha256: Some(&upper_sha),
                pre_size_bytes: Some(1),
            })
            .is_err());

        // Unknown tool_name or negative turn_index rejected.
        assert!(repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_id,
                run_id,
                step_id: None,
                turn_index: -1,
                workspace_root: "/ws",
                relative_path: "a.txt",
                tool_name: "write_file",
                pre_existed: false,
                pre_blob_sha256: None,
                pre_size_bytes: None,
            })
            .is_err());
        assert!(repo
            .insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: conv_id,
                run_id,
                step_id: None,
                turn_index: 0,
                workspace_root: "/ws",
                relative_path: "a.txt",
                tool_name: "run_shell",
                pre_existed: false,
                pre_blob_sha256: None,
                pre_size_bytes: None,
            })
            .is_err());
    }
}
