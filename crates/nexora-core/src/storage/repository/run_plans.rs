//! Durable agent run plan & decision repository (`run_plans`, v19).

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use crate::storage::database::{now_secs, Database};
use crate::storage::repository::{Repository, Result};

/// Plan run started/failed/interrupted before producing a completed plan.
pub const PLAN_STATUS_NONE: &str = "none";
/// Plan generated and awaiting user decision (`Yes` / `No` / `Custom reply`).
pub const PLAN_STATUS_PENDING: &str = "pending";
/// User approved starting implementation (`Yes`).
pub const PLAN_STATUS_ACCEPTED: &str = "accepted";
/// User dismissed/declined implementation (`No`).
pub const PLAN_STATUS_REJECTED: &str = "rejected";
/// User supplied custom implementation/steering instructions (`Custom reply`).
pub const PLAN_STATUS_CUSTOM_REPLY: &str = "custom_reply";

/// Persisted `run_plans` row for one Plan run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunPlan {
    /// Primary key of the plan row.
    pub id: i64,
    /// Owning Plan run (`agent_runs.id`), unique per plan run.
    pub run_id: i64,
    /// Owning conversation identifier.
    pub conversation_id: i64,
    /// Markdown plan outline generated once the Plan run completes (`None` while running or aborted).
    pub plan_content: Option<String>,
    /// Current user decision status (`none` | `pending` | `accepted` | `rejected` | `custom_reply`).
    pub decision_status: String,
    /// Optional custom reply text when `decision_status == 'custom_reply'`.
    pub decision_reply: Option<String>,
    /// Optional implementation run id started from this plan.
    pub implemented_run_id: Option<i64>,
    /// Unix timestamp (seconds) when the plan row was created.
    pub created_at: i64,
    /// Unix timestamp (seconds) when the user resolved the decision prompt.
    pub decided_at: Option<i64>,
}

const SELECT_COLUMNS: &str = "id, run_id, conversation_id, plan_content, decision_status, \
    decision_reply, implemented_run_id, created_at, decided_at";

fn map_plan(row: &Row<'_>) -> rusqlite::Result<RunPlan> {
    Ok(RunPlan {
        id: row.get(0)?,
        run_id: row.get(1)?,
        conversation_id: row.get(2)?,
        plan_content: row.get(3)?,
        decision_status: row.get(4)?,
        decision_reply: row.get(5)?,
        implemented_run_id: row.get(6)?,
        created_at: row.get(7)?,
        decided_at: row.get(8)?,
    })
}

/// Persistence adapter over `run_plans`.
pub struct RunPlanRepository<'a> {
    db: &'a Database,
}

impl Repository for RunPlanRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl<'a> RunPlanRepository<'a> {
    /// Construct a repository over the shared [`Database`].
    #[must_use]
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Record at run start that `run_id` is a read-only Plan run (`decision_status = 'none'`).
    ///
    /// # Errors
    /// Propagates any [`crate::storage::database::DatabaseError`].
    pub fn mark_started(&self, run_id: i64, conversation_id: i64) -> Result<RunPlan> {
        let now = now_secs();
        let row = self.transaction(|tx| {
            let existing = {
                let sql = format!("SELECT {SELECT_COLUMNS} FROM run_plans WHERE run_id = ?1");
                tx.query_row(&sql, [run_id], map_plan).optional()?
            };
            if let Some(found) = existing {
                return Ok(found);
            }
            tx.execute(
                "INSERT INTO run_plans \
                 (run_id, conversation_id, plan_content, decision_status, created_at) \
                 VALUES (?1, ?2, NULL, 'none', ?3)",
                params![run_id, conversation_id, now],
            )?;
            let id = tx.last_insert_rowid();
            let sql = format!("SELECT {SELECT_COLUMNS} FROM run_plans WHERE id = ?1");
            Ok(tx.query_row(&sql, [id], map_plan)?)
        })?;
        Ok(row)
    }

    /// Persist a completed Plan run's content idempotently and mark `decision_status = 'pending'`.
    ///
    /// Creates the row if `mark_started` was skipped (e.g. direct/test calls) and only
    /// promotes rows still in `'none'`.
    ///
    /// # Errors
    /// Propagates any [`crate::storage::database::DatabaseError`].
    pub fn create(&self, run_id: i64, conversation_id: i64, plan_content: &str) -> Result<RunPlan> {
        let now = now_secs();
        let row = self.transaction(|tx| {
            let existing = {
                let sql = format!("SELECT {SELECT_COLUMNS} FROM run_plans WHERE run_id = ?1");
                tx.query_row(&sql, [run_id], map_plan).optional()?
            };
            if let Some(found) = existing {
                if found.decision_status == PLAN_STATUS_NONE && found.plan_content.is_none() {
                    tx.execute(
                        "UPDATE run_plans SET plan_content = ?2, decision_status = 'pending' WHERE id = ?1",
                        params![found.id, plan_content],
                    )?;
                    let sql = format!("SELECT {SELECT_COLUMNS} FROM run_plans WHERE id = ?1");
                    return Ok(tx.query_row(&sql, [found.id], map_plan)?);
                }
                return Ok(found);
            }
            tx.execute(
                "INSERT INTO run_plans \
                 (run_id, conversation_id, plan_content, decision_status, created_at) \
                 VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![run_id, conversation_id, plan_content, now],
            )?;
            let id = tx.last_insert_rowid();
            let sql = format!("SELECT {SELECT_COLUMNS} FROM run_plans WHERE id = ?1");
            Ok(tx.query_row(&sql, [id], map_plan)?)
        })?;
        Ok(row)
    }

    /// Read the plan row for one `run_id`, if present.
    ///
    /// # Errors
    /// Propagates any [`crate::storage::database::DatabaseError`].
    pub fn read_by_run(&self, run_id: i64) -> Result<Option<RunPlan>> {
        let conn = self.read_conn()?;
        let sql = format!("SELECT {SELECT_COLUMNS} FROM run_plans WHERE run_id = ?1");
        let row = conn.query_row(&sql, [run_id], map_plan).optional()?;
        Ok(row)
    }

    /// List all persisted plan records for `conversation_id`, ordered oldest first (`id ASC`).
    ///
    /// # Errors
    /// Propagates any [`crate::storage::database::DatabaseError`].
    pub fn list_for_conversation(&self, conversation_id: i64) -> Result<Vec<RunPlan>> {
        let conn = self.read_conn()?;
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM run_plans WHERE conversation_id = ?1 ORDER BY id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([conversation_id], map_plan)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Record a user decision on `run_id`'s plan row (`pending` -> `accepted` | `rejected` | `custom_reply`).
    ///
    /// Returns `Ok(None)` if no plan exists for `run_id` or the plan was already decided.
    ///
    /// # Errors
    /// Propagates any [`crate::storage::database::DatabaseError`].
    pub fn decide(
        &self,
        run_id: i64,
        decision_status: &str,
        decision_reply: Option<&str>,
        implemented_run_id: Option<i64>,
    ) -> Result<Option<RunPlan>> {
        let now = now_secs();
        let updated = self.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE run_plans \
                 SET decision_status = ?2, \
                     decision_reply = ?3, \
                     implemented_run_id = ?4, \
                     decided_at = ?5 \
                 WHERE run_id = ?1 AND decision_status = 'pending'",
                params![
                    run_id,
                    decision_status,
                    decision_reply,
                    implemented_run_id,
                    now
                ],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            let sql = format!("SELECT {SELECT_COLUMNS} FROM run_plans WHERE run_id = ?1");
            Ok(tx.query_row(&sql, [run_id], map_plan).optional()?)
        })?;
        Ok(updated)
    }

    /// Attach the launched `implemented_run_id` to an already decided plan row.
    ///
    /// # Errors
    /// Propagates any [`crate::storage::database::DatabaseError`].
    pub fn set_implemented_run(&self, run_id: i64, implemented_run_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE run_plans SET implemented_run_id = ?2 WHERE run_id = ?1",
            params![run_id, implemented_run_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::in_memory_database;
    use crate::storage::repository::agent_runs::AgentRunRepository;

    fn seed_conversation_and_plan_run(db: &Database) -> (i64, i64) {
        let conn = db.lock().expect("lock db");
        conn.execute("INSERT INTO conversations (title) VALUES ('Plan test')", [])
            .expect("insert conversation");
        let conv_id = conn.last_insert_rowid();
        drop(conn);

        let runs = AgentRunRepository::new(db);
        let run_id = runs
            .create_run(Some(conv_id), "test-model", "supervised")
            .expect("create plan run");
        (conv_id, run_id)
    }

    #[test]
    fn start_complete_read_list_and_decide_plan_round_trip() {
        let db = in_memory_database();
        let (conv_id, run_id) = seed_conversation_and_plan_run(&db);
        let repo = RunPlanRepository::new(&db);

        let started = repo.mark_started(run_id, conv_id).expect("mark started");
        assert_eq!(started.decision_status, PLAN_STATUS_NONE);
        assert_eq!(started.plan_content, None);

        AgentRunRepository::new(&db)
            .finalize_run(
                run_id,
                "completed",
                Some("1. Step one\n2. Step two"),
                None,
                None,
                None,
            )
            .expect("finalize plan run");

        let created = repo
            .create(run_id, conv_id, "1. Read files\n2. Edit target")
            .expect("complete plan");
        assert_eq!(created.id, started.id);
        assert_eq!(created.run_id, run_id);
        assert_eq!(created.conversation_id, conv_id);
        assert_eq!(created.decision_status, PLAN_STATUS_PENDING);
        assert_eq!(
            created.plan_content.as_deref(),
            Some("1. Read files\n2. Edit target")
        );
        assert_eq!(created.decision_reply, None);
        assert_eq!(created.implemented_run_id, None);
        assert!(created.decided_at.is_none());

        // Idempotent repeat create returns existing row untouched.
        let again = repo
            .create(run_id, conv_id, "different text")
            .expect("idempotent create");
        assert_eq!(again.id, created.id);
        assert_eq!(again.plan_content, created.plan_content);

        let listed = repo.list_for_conversation(conv_id).expect("list plans");
        assert_eq!(listed, vec![created.clone()]);

        let runs = AgentRunRepository::new(&db);
        let impl_run_id = runs
            .create_run(Some(conv_id), "test-model", "semi_autonomous")
            .expect("create impl run");
        runs.record_continuation(impl_run_id, run_id, conv_id)
            .expect("record continuation");
        assert_eq!(
            runs.read_continuation_parent(impl_run_id)
                .expect("read parent"),
            Some(run_id)
        );
        assert_eq!(
            runs.list_continuations_for_conversation(conv_id)
                .expect("list continuations"),
            vec![(impl_run_id, run_id)]
        );

        let decided = repo
            .decide(run_id, PLAN_STATUS_ACCEPTED, None, Some(impl_run_id))
            .expect("decide accepted")
            .expect("plan existed and was pending");
        assert_eq!(decided.decision_status, PLAN_STATUS_ACCEPTED);
        assert_eq!(decided.implemented_run_id, Some(impl_run_id));
        assert!(decided.decided_at.is_some());

        // Second decide on already-decided plan returns None without overwriting.
        let repeat = repo
            .decide(run_id, PLAN_STATUS_REJECTED, None, None)
            .expect("repeat decide");
        assert!(repeat.is_none());
        let stored = repo
            .read_by_run(run_id)
            .expect("read plan")
            .expect("plan present");
        assert_eq!(stored.decision_status, PLAN_STATUS_ACCEPTED);
    }

    #[test]
    fn custom_reply_persists_reply_text() {
        let db = in_memory_database();
        let (conv_id, run_id) = seed_conversation_and_plan_run(&db);
        let repo = RunPlanRepository::new(&db);

        repo.create(run_id, conv_id, "1. Plan A")
            .expect("create plan");
        let updated = repo
            .decide(
                run_id,
                PLAN_STATUS_CUSTOM_REPLY,
                Some("Do step 1 with tests"),
                None,
            )
            .expect("decide custom")
            .expect("pending row updated");
        assert_eq!(updated.decision_status, PLAN_STATUS_CUSTOM_REPLY);
        assert_eq!(
            updated.decision_reply.as_deref(),
            Some("Do step 1 with tests")
        );
    }
}
