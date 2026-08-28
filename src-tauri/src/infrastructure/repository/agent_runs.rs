//! Agent run repository: persistence for the `agent_runs` and `agent_steps`
//! tables (DATABASE.md §7.8, §7.9; agent roadmap, Task 4.2).
//!
//! `agent_runs` stores one row per opt-in-persisted agent run (the multi-step
//! `ReAct` loop with governance); `agent_steps` stores the structured step
//! records appended during the run (D12). This repository is the only
//! data-access path for those tables and reuses the [`Repository`]
//! foundation, so it never duplicates connection or transaction handling, nor
//! error conversion.
//!
//! This repository is responsible **only** for persistence: it stores and
//! retrieves rows without interpreting them. Run lifecycle policy (when a
//! run starts, when it finalizes, which mode is recorded) lives in the
//! application layer's run recorder
//! ([`crate::application::agent::persistence`]).
//!
//! Per DATABASE.md §7.8–§7.9:
//!
//! - A run's terminal fields (`status`, `finished_at`, `total_steps`,
//!   `final_content`, `error`) are the only mutable fields, set at run
//!   termination by [`AgentRunRepository::finalize_run`].
//! - `agent_steps` is append-only and immutable after insertion; the
//!   repository exposes no update method for it.
//! - Deletion cascades are schema-enforced: deleting a run removes its steps,
//!   and deleting a conversation removes its runs (and their steps) per the
//!   D50 privacy doctrine (DATABASE.md §9).

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::{params, Error as SqliteError};
use serde::Serialize;

/// A single `agent_runs` row as persisted, mirroring the columns defined by
/// DATABASE.md §7.8. It is a plain persistence record and carries no
/// interpretation or business meaning; `mode` and `status` hold the column
/// values (`'supervised'` / `'semi_autonomous'` / `'full_autonomous'` and
/// `'running'` / `'completed'` / `'cancelled'` / `'budget_exhausted'` /
/// `'error'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentRun {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Owning conversation (`conversation_id`), `None` until the Task 5.1
    /// IPC layer wires runs to conversations (D50).
    pub conversation_id: Option<i64>,
    /// Provider model name for the run (`model`). Never a credential.
    pub model: String,
    /// Autonomy mode at run start (`mode`), stored as the column value.
    pub mode: String,
    /// Run state (`status`), stored as the column value.
    pub status: String,
    /// Start timestamp (`started_at`).
    pub started_at: i64,
    /// Termination timestamp (`finished_at`), `None` while the run is active.
    pub finished_at: Option<i64>,
    /// Number of recorded steps (`total_steps`).
    pub total_steps: i64,
    /// Final assistant text (`final_content`), terminal `completed` only.
    pub final_content: Option<String>,
    /// Classified error text (`error`), terminal `error` only. Never a
    /// secret (DATABASE.md §14).
    pub error: Option<String>,
}

/// A single `agent_steps` row as persisted, mirroring the columns defined by
/// DATABASE.md §7.9. It is a plain persistence record and carries no
/// interpretation or business meaning; `kind` holds the column value
/// (`'model_turn'` / `'tool_call'` / `'approval'`) and `status` the optional
/// tool call outcome (`'succeeded'` / `'failed'` / `'denied'` /
/// `'cancelled'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentStep {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Owning run (`run_id`).
    pub run_id: i64,
    /// 1-based step sequence within the run (`seq`).
    pub seq: i64,
    /// Step kind (`kind`), stored as the column value.
    pub kind: String,
    /// Tool name (`tool_name`), `None` for `model_turn` steps.
    pub tool_name: Option<String>,
    /// Raw JSON arguments exactly as provider-supplied (`arguments`).
    pub arguments: Option<String>,
    /// Tool output / denial text / approval decision (`observation`).
    pub observation: Option<String>,
    /// Tool call outcome (`status`), stored as the column value.
    pub status: Option<String>,
    /// Step start timestamp (`started_at`).
    pub started_at: i64,
    /// Step duration in milliseconds (`duration_ms`).
    pub duration_ms: Option<i64>,
}

/// Repository for the `agent_runs` and `agent_steps` tables.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct AgentRunRepository<'a> {
    db: &'a Database,
}

impl<'a> AgentRunRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for AgentRunRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl AgentRunRepository<'_> {
    /// Insert a new run row at run start (DATABASE.md §7.8).
    ///
    /// Persists the caller-supplied `conversation_id` (`None` until the
    /// Task 5.1 IPC layer wires runs to conversations), `model`, and `mode`.
    /// The schema defaults assign `status = 'running'`,
    /// `started_at = unixepoch()`, and `total_steps = 0`; the surrogate `id`
    /// is assigned by the schema.
    ///
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a missing
    /// `conversation_id` (foreign-key violation) or a `model` / `mode` value
    /// rejected by the table CHECK constraints.
    pub(crate) fn create_run(
        &self,
        conversation_id: Option<i64>,
        model: &str,
        mode: &str,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO agent_runs (conversation_id, model, mode) VALUES (?1, ?2, ?3)",
            params![conversation_id, model, mode],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Finalize a run at termination (DATABASE.md §7.8).
    ///
    /// Updates the terminal fields only — `status`, `finished_at` (set to the
    /// current Unix timestamp), `total_steps`, `final_content`, and `error` —
    /// exactly the fields the documented Update path allows. `final_content`
    /// is supplied for terminal `completed` runs only and `error` (classified
    /// text, never a secret) for terminal `error` runs only.
    ///
    /// Finalizing a non-existent `id` is a no-op.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails, for example a
    /// `status` value rejected by the table CHECK constraint.
    pub(crate) fn finalize_run(
        &self,
        id: i64,
        status: &str,
        total_steps: i64,
        final_content: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE agent_runs SET status = ?2, finished_at = (unixepoch()), \
             total_steps = ?3, final_content = ?4, error = ?5 WHERE id = ?1",
            params![id, status, total_steps, final_content, error],
        )?;
        Ok(())
    }

    /// Read one run by `id`.
    ///
    /// Returns `Ok(None)` when no run with `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the read fails.
    pub(crate) fn read_run(&self, id: i64) -> Result<Option<AgentRun>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, model, mode, status, started_at, finished_at, \
             total_steps, final_content, error FROM agent_runs WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_agent_run)?;
        rows.next().transpose().map_err(DatabaseError::Sqlite)
    }

    /// List all runs ordered by `started_at` descending — the run history
    /// listing by recency (DATABASE.md §7.8, §8).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list_runs_by_started_at_desc(&self) -> Result<Vec<AgentRun>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, model, mode, status, started_at, finished_at, \
             total_steps, final_content, error FROM agent_runs ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_agent_run)?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    /// List the runs of one conversation ordered by `started_at` descending
    /// (DATABASE.md §7.8, §8).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list_runs_by_conversation(&self, conversation_id: i64) -> Result<Vec<AgentRun>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, model, mode, status, started_at, finished_at, \
             total_steps, final_content, error FROM agent_runs \
             WHERE conversation_id = ?1 ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([conversation_id], row_to_agent_run)?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    /// Delete a run by `id` (DATABASE.md §7.8).
    ///
    /// Deleting a non-existent `id` is a no-op. Cascading deletion of the
    /// run's steps (`agent_steps`) and of the runs of a deleted conversation
    /// is enforced by the schema's foreign keys (DATABASE.md §9) and is not
    /// handled here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete_run(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM agent_runs WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Append one step record to a run (DATABASE.md §7.9).
    ///
    /// Persists the caller-supplied `run_id`, `seq`, `kind`, `tool_name`,
    /// `arguments` (raw JSON exactly as provider-supplied), `observation`,
    /// `status`, and `duration_ms`. The schema defaults assign
    /// `started_at = unixepoch()`; the surrogate `id` is assigned by the
    /// schema. Callers own the step ordering: `seq` must be monotonically
    /// increasing per run, and the schema's `UNIQUE(run_id, seq)` rejects
    /// duplicates.
    ///
    /// Steps are immutable after insertion; the repository exposes no update
    /// method for them (DATABASE.md §7.9).
    ///
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a
    /// missing `run_id` (foreign-key violation), a duplicate
    /// `UNIQUE(run_id, seq)`, or a `kind` / `status` / `tool_name` /
    /// `duration_ms` value rejected by the table CHECK constraints.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_step(
        &self,
        run_id: i64,
        seq: i64,
        kind: &str,
        tool_name: Option<&str>,
        arguments: Option<&str>,
        observation: Option<&str>,
        status: Option<&str>,
        duration_ms: Option<i64>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO agent_steps \
                 (run_id, seq, kind, tool_name, arguments, observation, status, duration_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run_id,
                seq,
                kind,
                tool_name,
                arguments,
                observation,
                status,
                duration_ms
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// List the recorded steps of one run ordered by `seq` (DATABASE.md
    /// §7.9, §8).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list_steps(&self, run_id: i64) -> Result<Vec<AgentStep>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, run_id, seq, kind, tool_name, arguments, observation, status, \
             started_at, duration_ms FROM agent_steps WHERE run_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            Ok(AgentStep {
                id: row.get(0)?,
                run_id: row.get(1)?,
                seq: row.get(2)?,
                kind: row.get(3)?,
                tool_name: row.get(4)?,
                arguments: row.get(5)?,
                observation: row.get(6)?,
                status: row.get(7)?,
                started_at: row.get(8)?,
                duration_ms: row.get(9)?,
            })
        })?;
        let mut steps = Vec::new();
        for row in rows {
            steps.push(row?);
        }
        Ok(steps)
    }
}

/// Map one `agent_runs` row onto an [`AgentRun`] record.
fn row_to_agent_run(row: &rusqlite::Row<'_>) -> std::result::Result<AgentRun, SqliteError> {
    Ok(AgentRun {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        model: row.get(2)?,
        mode: row.get(3)?,
        status: row.get(4)?,
        started_at: row.get(5)?,
        finished_at: row.get(6)?,
        total_steps: row.get(7)?,
        final_content: row.get(8)?,
        error: row.get(9)?,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    fn repo(db: &Database) -> AgentRunRepository<'_> {
        AgentRunRepository::new(db)
    }

    #[test]
    fn create_read_finalize_round_trip_persists_terminal_fields() {
        let db = in_memory_database();
        let runs = repo(&db);

        let run_id = runs
            .create_run(None, "gpt-test", "supervised")
            .expect("create run");

        let created = runs
            .read_run(run_id)
            .expect("read created")
            .expect("exists");
        assert_eq!(created.id, run_id);
        assert_eq!(created.conversation_id, None);
        assert_eq!(created.model, "gpt-test");
        assert_eq!(created.mode, "supervised");
        assert_eq!(created.status, "running");
        assert_eq!(created.total_steps, 0);
        assert_eq!(created.finished_at, None);

        runs.finalize_run(run_id, "completed", 3, Some("all done"), None)
            .expect("finalize run");

        let finalized = runs
            .read_run(run_id)
            .expect("read finalized")
            .expect("exists");
        assert_eq!(finalized.status, "completed");
        assert_eq!(finalized.total_steps, 3);
        assert_eq!(finalized.final_content.as_deref(), Some("all done"));
        assert_eq!(finalized.error, None);
        assert!(finalized.finished_at.is_some(), "finalize stamps the time");
    }

    #[test]
    fn steps_append_in_order_and_list_by_seq() {
        let db = in_memory_database();
        let runs = repo(&db);

        let run_id = runs.create_run(None, "m", "full_autonomous").expect("run");
        let step1 = runs
            .append_step(
                run_id,
                1,
                "model_turn",
                None,
                None,
                Some("thinking"),
                None,
                None,
            )
            .expect("step 1");
        let step2 = runs
            .append_step(
                run_id,
                2,
                "tool_call",
                Some("write_file"),
                Some("{\"path\":\"a.txt\"}"),
                Some("wrote 5 bytes"),
                Some("succeeded"),
                Some(12),
            )
            .expect("step 2");

        let all_steps = runs.list_steps(run_id).expect("list steps");
        assert_eq!(all_steps.len(), 2);
        assert_eq!(
            all_steps.iter().map(|s| s.seq).collect::<Vec<_>>(),
            vec![1, 2],
            "steps list in seq order"
        );
        assert_eq!(all_steps[0].id, step1);
        assert_eq!(all_steps[1].id, step2);
        assert_eq!(all_steps[1].tool_name.as_deref(), Some("write_file"));
        assert_eq!(all_steps[1].status.as_deref(), Some("succeeded"));
        assert_eq!(all_steps[1].duration_ms, Some(12));
    }

    #[test]
    fn duplicate_seq_within_a_run_is_rejected() {
        let db = in_memory_database();
        let runs = repo(&db);

        let run_id = runs.create_run(None, "m", "supervised").expect("run");
        runs.append_step(run_id, 1, "model_turn", None, None, None, None, None)
            .expect("first step");
        let duplicate = runs.append_step(
            run_id,
            1,
            "tool_call",
            Some("read_file"),
            None,
            None,
            None,
            None,
        );
        assert!(
            duplicate.is_err(),
            "UNIQUE(run_id, seq) must reject a duplicate seq"
        );
    }

    #[test]
    fn deleting_a_run_cascades_its_steps() {
        let db = in_memory_database();
        let runs = repo(&db);

        let run_id = runs.create_run(None, "m", "supervised").expect("run");
        runs.append_step(run_id, 1, "model_turn", None, None, None, None, None)
            .expect("step 1");
        runs.append_step(run_id, 2, "model_turn", None, None, None, None, None)
            .expect("step 2");

        runs.delete_run(run_id).expect("delete run");
        assert!(runs.read_run(run_id).expect("read").is_none());
        assert!(
            runs.list_steps(run_id).expect("list steps").is_empty(),
            "run delete cascades its steps"
        );
    }

    #[test]
    fn conversation_delete_cascades_runs_and_steps() {
        let db = in_memory_database();
        let runs = repo(&db);
        let conversations =
            crate::infrastructure::repository::conversations::ConversationRepository::new(&db);

        let conv_id = conversations
            .create("c", "active")
            .expect("create conversation");
        let run_id = runs
            .create_run(Some(conv_id), "m", "semi_autonomous")
            .expect("linked run");
        runs.append_step(run_id, 1, "model_turn", None, None, None, None, None)
            .expect("step");

        let listed = runs
            .list_runs_by_conversation(conv_id)
            .expect("list by conversation");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].conversation_id, Some(conv_id));

        conversations.delete(conv_id).expect("delete conversation");
        assert!(
            runs.list_runs_by_conversation(conv_id)
                .expect("list after cascade")
                .is_empty(),
            "conversation delete cascades agent runs"
        );
        assert!(
            runs.list_steps(run_id).expect("list steps").is_empty(),
            "conversation delete cascades agent steps through the run"
        );
    }

    #[test]
    fn check_constraints_reject_invalid_values() {
        let db = in_memory_database();
        let runs = repo(&db);

        assert!(
            runs.create_run(None, "", "supervised").is_err(),
            "empty model must be rejected"
        );
        assert!(
            runs.create_run(None, "m", "chaos").is_err(),
            "unknown mode must be rejected"
        );

        let run_id = runs.create_run(None, "m", "supervised").expect("run");
        assert!(
            runs.finalize_run(run_id, "transcended", 1, None, None)
                .is_err(),
            "unknown run status must be rejected"
        );
        assert!(
            runs.append_step(run_id, 1, "divination", None, None, None, None, None)
                .is_err(),
            "unknown step kind must be rejected"
        );
        assert!(
            runs.append_step(run_id, 0, "model_turn", None, None, None, None, None)
                .is_err(),
            "seq < 1 must be rejected"
        );
        assert!(
            runs.append_step(
                run_id,
                1,
                "tool_call",
                Some("read_file"),
                None,
                None,
                Some("meh"),
                None,
            )
            .is_err(),
            "unknown step status must be rejected"
        );
        assert!(
            runs.append_step(run_id, 2, "tool_call", Some(""), None, None, None, None)
                .is_err(),
            "empty tool_name must be rejected"
        );
        assert!(
            runs.append_step(
                run_id,
                2,
                "tool_call",
                Some("read_file"),
                None,
                None,
                None,
                Some(-1),
            )
            .is_err(),
            "negative duration_ms must be rejected"
        );
    }

    #[test]
    fn runs_list_by_recency() {
        let db = in_memory_database();
        let runs = repo(&db);

        let first = runs.create_run(None, "m", "supervised").expect("run 1");
        let second = runs.create_run(None, "m", "supervised").expect("run 2");
        let listed = runs.list_runs_by_started_at_desc().expect("list runs");
        let ids: Vec<i64> = listed.iter().map(|r| r.id).collect();
        assert!(ids.contains(&first) && ids.contains(&second));
        assert_eq!(listed.len(), 2);
    }
}
