//! Task-manager repository: persistence for the `agent_tasks` and
//! `agent_task_steps` tables (v8 migration; task manager + autonomous mode).
//!
//! `agent_tasks` stores one row per user-defined task list (title, optional
//! description, lifecycle status, the backing conversation that the
//! autonomous loop's agent runs execute inside of, the provider/model the
//! runs use, the step cap, progress counters, the composed report, and the
//! latest step's agent-run link); `agent_task_steps` stores the ordered,
//! individually completable steps (title, lifecycle status, per-step result,
//! and the step's own agent-run link).
//!
//! This repository is responsible **only** for persistence: it stores and
//! retrieves rows without interpreting them. Task lifecycle policy (when a
//! task starts, how the autonomous loop advances steps, how the report is
//! composed) lives in the application layer
//! ([`crate::application::agent::tasks`]).
//!
//! - A task's mutable fields (`status`, `current_step`, `total_steps`,
//!   `report`, `run_id`) are set by [`AgentTaskRepository::mark_task_*`]
//!   during the autonomous loop; `updated_at` is refreshed explicitly on
//!   every such write (no trigger: only these writes touch it).
//! - `agent_task_steps` rows are created once per task (one per `seq`) and
//!   then only transitioned by [`AgentTaskRepository::mark_step_*`].
//! - Deletion cascades are schema-enforced: deleting a task removes its
//!   steps; deleting a conversation removes its tasks (and through them
//!   their steps) per the D50 privacy doctrine; agent-run links are
//!   `ON DELETE SET NULL` so run history survives task deletion.

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::{params, Error as SqliteError};
use serde::Serialize;

/// A single `agent_tasks` row as persisted. It is a plain persistence record
/// and carries no interpretation; `status` holds the column value
/// (`'pending'` / `'running'` / `'completed'` / `'failed'` / `'cancelled'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentTask {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Task title (`title`, 1..=200 chars).
    pub title: String,
    /// Optional longer description (`description`, `None` when absent).
    pub description: Option<String>,
    /// Lifecycle status (`status`), stored as the column value.
    pub status: String,
    /// Backing conversation the autonomous runs execute inside of
    /// (`conversation_id`); `None` until the task is wired to one.
    pub conversation_id: Option<i64>,
    /// Provider internal name for the autonomous runs (`provider`).
    pub provider: Option<String>,
    /// Model name for the autonomous runs (`model`). Never a credential.
    pub model: Option<String>,
    /// Step cap for the autonomous loop (`max_steps`, 1..=25).
    pub max_steps: i64,
    /// 1-based index of the step currently executing (`current_step`).
    pub current_step: i64,
    /// Total steps defined for the task (`total_steps`).
    pub total_steps: i64,
    /// Composed plan → act → verify → report text (`report`).
    pub report: Option<String>,
    /// Latest step's agent-run link (`run_id`), `None` before the first run.
    pub run_id: Option<i64>,
    /// Creation timestamp (`created_at`).
    pub created_at: i64,
    /// Last mutation timestamp (`updated_at`).
    pub updated_at: i64,
}

/// A single `agent_task_steps` row as persisted. `status` holds the column
/// value (`'pending'` / `'running'` / `'completed'` / `'failed'` /
/// `'skipped'` / `'cancelled'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentTaskStep {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Owning task (`task_id`).
    pub task_id: i64,
    /// 1-based step sequence within the task (`seq`).
    pub seq: i64,
    /// Step title (`title`, 1..=500 chars).
    pub title: String,
    /// Lifecycle status (`status`), stored as the column value.
    pub status: String,
    /// Per-step outcome text (`result`), set when the step terminates.
    pub result: Option<String>,
    /// The step's own agent-run link (`run_id`).
    pub run_id: Option<i64>,
    /// Step start timestamp (`started_at`).
    pub started_at: i64,
    /// Step termination timestamp (`finished_at`), `None` while active.
    pub finished_at: Option<i64>,
}

/// Repository for the `agent_tasks` and `agent_task_steps` tables.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct AgentTaskRepository<'a> {
    db: &'a Database,
}

impl<'a> AgentTaskRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for AgentTaskRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl AgentTaskRepository<'_> {
    /// Insert a new task row (status `'pending'`, progress counters zeroed).
    ///
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a title
    /// rejected by the table CHECK constraints or a missing
    /// `conversation_id` (foreign-key violation).
    pub(crate) fn create_task(
        &self,
        title: &str,
        description: Option<&str>,
        conversation_id: Option<i64>,
        provider: Option<&str>,
        model: Option<&str>,
        max_steps: i64,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO agent_tasks (title, description, conversation_id, provider, model, max_steps) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                title,
                description,
                conversation_id,
                provider,
                model,
                max_steps
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Read one task by `id`. Returns `Ok(None)` when no task exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the read fails.
    pub(crate) fn read_task(&self, id: i64) -> Result<Option<AgentTask>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, description, status, conversation_id, provider, model, \
              max_steps, current_step, total_steps, report, run_id, created_at, updated_at \
             FROM agent_tasks WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_agent_task)?;
        rows.next().transpose().map_err(DatabaseError::Sqlite)
    }

    /// List all tasks ordered by `updated_at` descending (most recently
    /// active first).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list_tasks(&self) -> Result<Vec<AgentTask>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, description, status, conversation_id, provider, model, \
              max_steps, current_step, total_steps, report, run_id, created_at, updated_at \
             FROM agent_tasks ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], row_to_agent_task)?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    /// Update a task's title/description (user edit; pending tasks only by
    /// caller contract — the repository enforces no status precondition).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails.
    pub(crate) fn update_task(
        &self,
        id: i64,
        title: &str,
        description: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE agent_tasks SET title = ?2, description = ?3, updated_at = (unixepoch()) WHERE id = ?1",
            params![id, title, description],
        )?;
        Ok(())
    }

    /// Mark a task `running` at loop start (resets progress counters and
    /// clears any previous report).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails.
    pub(crate) fn mark_task_running(&self, id: i64, total_steps: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE agent_tasks SET status = 'running', current_step = 0, total_steps = ?2, \
              report = NULL, run_id = NULL, updated_at = (unixepoch()) WHERE id = ?1",
            params![id, total_steps],
        )?;
        Ok(())
    }

    /// Record loop progress after each step (current step index + latest run
    /// link).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails.
    pub(crate) fn mark_task_progress(
        &self,
        id: i64,
        current_step: i64,
        run_id: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE agent_tasks SET current_step = ?2, run_id = ?3, updated_at = (unixepoch()) WHERE id = ?1",
            params![id, current_step, run_id],
        )?;
        Ok(())
    }

    /// Finalize a task at loop termination with its terminal status and the
    /// composed report.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails, for example a
    /// `status` value rejected by the table CHECK constraint.
    pub(crate) fn finalize_task(&self, id: i64, status: &str, report: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE agent_tasks SET status = ?2, report = ?3, updated_at = (unixepoch()) WHERE id = ?1",
            params![id, status, report],
        )?;
        Ok(())
    }

    /// Delete a task by `id`. Deleting a non-existent `id` is a no-op.
    /// Cascading deletion of the task's steps is schema-enforced.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete_task(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM agent_tasks WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Append one step record to a task. Callers own the ordering: `seq`
    /// must increase per task; `UNIQUE(task_id, seq)` rejects duplicates.
    ///
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a
    /// missing `task_id` (foreign-key violation) or a duplicate
    /// `UNIQUE(task_id, seq)`.
    pub(crate) fn append_step(&self, task_id: i64, seq: i64, title: &str) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO agent_task_steps (task_id, seq, title) VALUES (?1, ?2, ?3)",
            params![task_id, seq, title],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Mark one step `running` at its start (stamps `started_at`, clears any
    /// previous result).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails.
    pub(crate) fn mark_step_running(&self, task_id: i64, seq: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE agent_task_steps SET status = 'running', result = NULL, run_id = NULL, \
              started_at = (unixepoch()), finished_at = NULL WHERE task_id = ?1 AND seq = ?2",
            params![task_id, seq],
        )?;
        Ok(())
    }

    /// Mark one step terminal with its status, outcome text, and run link.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails, for example a
    /// `status` value rejected by the table CHECK constraint.
    pub(crate) fn mark_step_finished(
        &self,
        task_id: i64,
        seq: i64,
        status: &str,
        result: Option<&str>,
        run_id: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE agent_task_steps SET status = ?3, result = ?4, run_id = ?5, \
              finished_at = (unixepoch()) WHERE task_id = ?1 AND seq = ?2",
            params![task_id, seq, status, result, run_id],
        )?;
        Ok(())
    }

    /// Reset every step of a task to `pending` (loop start: clears any
    /// previous attempt's states, results, and run links).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails.
    pub(crate) fn reset_steps(&self, task_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE agent_task_steps SET status = 'pending', result = NULL, run_id = NULL, \
              started_at = (unixepoch()), finished_at = NULL WHERE task_id = ?1",
            [task_id],
        )?;
        Ok(())
    }

    /// List the steps of one task ordered by `seq`.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list_steps(&self, task_id: i64) -> Result<Vec<AgentTaskStep>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, seq, title, status, result, run_id, started_at, finished_at \
             FROM agent_task_steps WHERE task_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([task_id], |row| {
            Ok(AgentTaskStep {
                id: row.get(0)?,
                task_id: row.get(1)?,
                seq: row.get(2)?,
                title: row.get(3)?,
                status: row.get(4)?,
                result: row.get(5)?,
                run_id: row.get(6)?,
                started_at: row.get(7)?,
                finished_at: row.get(8)?,
            })
        })?;
        let mut steps = Vec::new();
        for row in rows {
            steps.push(row?);
        }
        Ok(steps)
    }
}

/// Map one `agent_tasks` row onto an [`AgentTask`] record.
fn row_to_agent_task(row: &rusqlite::Row<'_>) -> std::result::Result<AgentTask, SqliteError> {
    Ok(AgentTask {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: row.get(3)?,
        conversation_id: row.get(4)?,
        provider: row.get(5)?,
        model: row.get(6)?,
        max_steps: row.get(7)?,
        current_step: row.get(8)?,
        total_steps: row.get(9)?,
        report: row.get(10)?,
        run_id: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    fn repo(db: &Database) -> AgentTaskRepository<'_> {
        AgentTaskRepository::new(db)
    }

    #[test]
    fn create_read_update_delete_round_trip() {
        let db = in_memory_database();
        let tasks = repo(&db);

        let id = tasks
            .create_task("t", None, None, None, None, 25)
            .expect("create");
        let created = tasks.read_task(id).expect("read").expect("exists");
        assert_eq!(created.title, "t");
        assert_eq!(created.status, "pending");
        assert_eq!(created.max_steps, 25);
        assert_eq!(created.current_step, 0);
        assert_eq!(created.total_steps, 0);
        assert_eq!(created.report, None);

        tasks
            .update_task(id, "renamed", Some("desc"))
            .expect("update");
        let updated = tasks.read_task(id).expect("read").expect("exists");
        assert_eq!(updated.title, "renamed");
        assert_eq!(updated.description.as_deref(), Some("desc"));

        tasks.delete_task(id).expect("delete");
        assert!(tasks.read_task(id).expect("read").is_none());
    }

    #[test]
    fn steps_append_in_order_and_list_by_seq() {
        let db = in_memory_database();
        let tasks = repo(&db);

        let id = tasks
            .create_task("task", None, None, None, None, 25)
            .expect("task");
        tasks.append_step(id, 1, "first").expect("step 1");
        tasks.append_step(id, 2, "second").expect("step 2");

        let steps = tasks.list_steps(id).expect("list");
        assert_eq!(steps.iter().map(|s| s.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(steps[0].status, "pending");

        tasks.mark_step_running(id, 1).expect("running");
        tasks
            .mark_step_finished(id, 1, "completed", Some("done"), None)
            .expect("finished");
        let steps = tasks.list_steps(id).expect("list");
        assert_eq!(steps[0].status, "completed");
        assert_eq!(steps[0].result.as_deref(), Some("done"));
        assert!(steps[0].finished_at.is_some());
    }

    #[test]
    fn duplicate_seq_within_a_task_is_rejected() {
        let db = in_memory_database();
        let tasks = repo(&db);

        let id = tasks
            .create_task("t", None, None, None, None, 25)
            .expect("task");
        tasks.append_step(id, 1, "one").expect("step 1");
        assert!(
            tasks.append_step(id, 1, "dup").is_err(),
            "UNIQUE(task_id, seq) must reject a duplicate seq"
        );
    }

    #[test]
    fn check_constraints_reject_invalid_values() {
        let db = in_memory_database();
        let tasks = repo(&db);

        assert!(
            tasks.create_task("", None, None, None, None, 25).is_err(),
            "empty title must be rejected"
        );
        assert!(
            tasks.create_task("t", None, None, None, None, 0).is_err(),
            "max_steps < 1 must be rejected"
        );
        assert!(
            tasks.create_task("t", None, None, None, None, 26).is_err(),
            "max_steps > 25 must be rejected"
        );

        let id = tasks
            .create_task("t", None, None, None, None, 25)
            .expect("task");
        assert!(
            tasks.finalize_task(id, "transcended", "r").is_err(),
            "unknown task status must be rejected"
        );
        tasks.append_step(id, 1, "one").expect("step");
        assert!(
            tasks
                .mark_step_finished(id, 1, "transcended", None, None)
                .is_err(),
            "unknown step status must be rejected"
        );
        assert!(
            tasks.append_step(id, 2, "").is_err(),
            "empty step title must be rejected"
        );
    }

    #[test]
    fn deleting_a_task_cascades_its_steps() {
        let db = in_memory_database();
        let tasks = repo(&db);

        let id = tasks
            .create_task("t", None, None, None, None, 25)
            .expect("task");
        tasks.append_step(id, 1, "one").expect("step");
        tasks.delete_task(id).expect("delete");
        assert!(
            tasks.list_steps(id).expect("list").is_empty(),
            "task delete cascades its steps"
        );
    }

    #[test]
    fn conversation_delete_cascades_tasks_and_steps() {
        let db = in_memory_database();
        let tasks = repo(&db);
        let conversations =
            crate::infrastructure::repository::conversations::ConversationRepository::new(&db);

        let conv_id = conversations.create("c", "active").expect("conversation");
        let id = tasks
            .create_task("t", None, Some(conv_id), None, None, 25)
            .expect("linked task");
        tasks.append_step(id, 1, "one").expect("step");

        conversations.delete(conv_id).expect("delete conversation");
        assert!(
            tasks.read_task(id).expect("read").is_none(),
            "conversation delete cascades tasks"
        );
        assert!(
            tasks.list_steps(id).expect("list").is_empty(),
            "conversation delete cascades task steps through the task"
        );
    }

    #[test]
    fn orphan_task_references_are_rejected() {
        let db = in_memory_database();
        let tasks = repo(&db);

        assert!(
            tasks
                .create_task("t", None, Some(999), None, None, 25)
                .is_err(),
            "missing conversation must be rejected"
        );
        assert!(
            tasks.append_step(999, 1, "one").is_err(),
            "missing task must be rejected"
        );
    }

    #[test]
    fn progress_and_finalize_markers_round_trip() {
        let db = in_memory_database();
        let tasks = repo(&db);

        let id = tasks
            .create_task("t", None, None, None, None, 25)
            .expect("task");
        tasks.mark_task_running(id, 3).expect("running");
        let running = tasks.read_task(id).expect("read").expect("exists");
        assert_eq!(running.status, "running");
        assert_eq!(running.total_steps, 3);
        assert_eq!(running.report, None);

        tasks.mark_task_progress(id, 2, None).expect("progress");
        let progressed = tasks.read_task(id).expect("read").expect("exists");
        assert_eq!(progressed.current_step, 2);

        tasks
            .finalize_task(id, "completed", "report text")
            .expect("finalize");
        let done = tasks.read_task(id).expect("read").expect("exists");
        assert_eq!(done.status, "completed");
        assert_eq!(done.report.as_deref(), Some("report text"));
    }
}
