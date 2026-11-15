//! Durable model-to-user clarifying questions repository (`run_questions`, v18).

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use crate::storage::database::{now_secs, Database};
use crate::storage::repository::{Repository, Result};

/// Persisted `run_questions` row for `ask_user_question`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunQuestion {
    /// Primary key of the question row.
    pub id: i64,
    /// Owning agent run identifier.
    pub run_id: i64,
    /// Owning conversation identifier.
    pub conversation_id: i64,
    /// Tool call identifier (`call_id`) unique within `run_id`.
    pub call_id: String,
    /// Prompt text shown to the user.
    pub question: String,
    /// Optional selectable choice labels.
    pub options: Vec<String>,
    /// Whether multiple choices can be selected.
    pub multi_select: bool,
    /// Whether free-text input is permitted.
    pub allow_free_text: bool,
    /// Question lifecycle status (`pending`, `answered`, or `cancelled`).
    pub status: String,
    /// User-selected options once answered.
    pub selected_options: Vec<String>,
    /// Optional free-text response provided by the user.
    pub free_text: Option<String>,
    /// Unix timestamp (seconds) when the question was created.
    pub created_at: i64,
    /// Unix timestamp (seconds) when the question was resolved.
    pub answered_at: Option<i64>,
}

/// Parameters required to insert a pending [`RunQuestion`].
#[derive(Debug, Clone, Copy)]
pub struct NewRunQuestion<'a> {
    /// Owning agent run identifier.
    pub run_id: i64,
    /// Owning conversation identifier.
    pub conversation_id: i64,
    /// Tool call identifier (`call_id`) unique within `run_id`.
    pub call_id: &'a str,
    /// Prompt text shown to the user.
    pub question: &'a str,
    /// Optional selectable choice labels.
    pub options: &'a [String],
    /// Whether multiple choices can be selected.
    pub multi_select: bool,
    /// Whether free-text input is permitted.
    pub allow_free_text: bool,
}

const SELECT_COLUMNS: &str = "id, run_id, conversation_id, call_id, question, options_json, \
    multi_select, allow_free_text, status, selected_options_json, free_text, created_at, answered_at";

fn decode_string_vec(raw: Option<String>) -> Vec<String> {
    raw.and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
        .unwrap_or_default()
}

fn map_question(row: &Row<'_>) -> rusqlite::Result<RunQuestion> {
    let options_json: String = row.get(5)?;
    let multi_select: i64 = row.get(6)?;
    let allow_free_text: i64 = row.get(7)?;
    let selected_json: Option<String> = row.get(9)?;
    Ok(RunQuestion {
        id: row.get(0)?,
        run_id: row.get(1)?,
        conversation_id: row.get(2)?,
        call_id: row.get(3)?,
        question: row.get(4)?,
        options: decode_string_vec(Some(options_json)),
        multi_select: multi_select != 0,
        allow_free_text: allow_free_text != 0,
        status: row.get(8)?,
        selected_options: decode_string_vec(selected_json),
        free_text: row.get(10)?,
        created_at: row.get(11)?,
        answered_at: row.get(12)?,
    })
}

/// Persistence adapter over `run_questions`.
pub struct RunQuestionRepository<'a> {
    db: &'a Database,
}

impl Repository for RunQuestionRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl<'a> RunQuestionRepository<'a> {
    /// Construct a repository bound to `db`.
    #[must_use]
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert a new pending question for `run_id` / `conversation_id` and return it.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) on `SQLite` errors or constraint violations.
    pub fn create_pending(&self, input: NewRunQuestion<'_>) -> Result<RunQuestion> {
        let now = now_secs();
        let options_json =
            serde_json::to_string(input.options).unwrap_or_else(|_| "[]".to_string());
        let id = self.transaction(|tx| {
            tx.execute(
                "INSERT INTO run_questions (
                    run_id, conversation_id, call_id, question, options_json,
                    multi_select, allow_free_text, status, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
                params![
                    input.run_id,
                    input.conversation_id,
                    input.call_id,
                    input.question,
                    options_json,
                    i64::from(input.multi_select),
                    i64::from(input.allow_free_text),
                    now,
                ],
            )?;
            Ok(tx.last_insert_rowid())
        })?;
        self.read_by_id(id)?.ok_or_else(|| {
            crate::storage::database::DatabaseError::Sqlite(rusqlite::Error::QueryReturnedNoRows)
        })
    }

    /// Fetch one question by primary key.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the query fails.
    pub fn read_by_id(&self, id: i64) -> Result<Option<RunQuestion>> {
        let conn = self.read_conn()?;
        let sql = format!("SELECT {SELECT_COLUMNS} FROM run_questions WHERE id = ?1");
        conn.query_row(&sql, [id], map_question)
            .optional()
            .map_err(Into::into)
    }

    /// Fetch the newest pending question for a run, if any.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the query fails.
    pub fn pending_for_run(&self, run_id: i64) -> Result<Option<RunQuestion>> {
        let conn = self.read_conn()?;
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM run_questions
             WHERE run_id = ?1 AND status = 'pending'
             ORDER BY id DESC LIMIT 1"
        );
        conn.query_row(&sql, [run_id], map_question)
            .optional()
            .map_err(Into::into)
    }

    /// List questions for a conversation in creation order.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the query fails.
    pub fn list_for_conversation(&self, conversation_id: i64) -> Result<Vec<RunQuestion>> {
        let conn = self.read_conn()?;
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM run_questions
             WHERE conversation_id = ?1
             ORDER BY id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([conversation_id], map_question)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// List pending questions for a conversation whose owning run is still `running`.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the query fails.
    pub fn list_pending_for_conversation(&self, conversation_id: i64) -> Result<Vec<RunQuestion>> {
        let conn = self.read_conn()?;
        let sql =
            "SELECT q.id, q.run_id, q.conversation_id, q.call_id, q.question, q.options_json, \
                    q.multi_select, q.allow_free_text, q.status, q.selected_options_json, \
                    q.free_text, q.created_at, q.answered_at
             FROM run_questions q
             JOIN agent_runs r ON r.id = q.run_id
             WHERE q.conversation_id = ?1 AND q.status = 'pending' AND r.status = 'running'
             ORDER BY q.id ASC";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([conversation_id], map_question)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Mark a pending question as answered. Returns `Ok(Some(updated))` when updated,
    /// `Ok(None)` when not found or no longer pending.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the update fails.
    pub fn answer_question(
        &self,
        question_id: i64,
        selected_options: &[String],
        free_text: Option<&str>,
    ) -> Result<Option<RunQuestion>> {
        let now = now_secs();
        let selected_json =
            serde_json::to_string(selected_options).unwrap_or_else(|_| "[]".to_string());
        let trimmed_free = free_text
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let changed = self.transaction(|tx| {
            let count = tx.execute(
                "UPDATE run_questions
                 SET status = 'answered',
                     selected_options_json = ?2,
                     free_text = ?3,
                     answered_at = ?4
                 WHERE id = ?1 AND status = 'pending'",
                params![question_id, selected_json, trimmed_free, now],
            )?;
            Ok(count)
        })?;
        if changed == 0 {
            return Ok(None);
        }
        self.read_by_id(question_id)
    }

    /// Cancel all pending questions belonging to `run_id`.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the update fails.
    pub fn cancel_pending_for_run(&self, run_id: i64) -> Result<usize> {
        let now = now_secs();
        self.transaction(|tx| {
            let count = tx.execute(
                "UPDATE run_questions
                 SET status = 'cancelled', answered_at = ?2
                 WHERE run_id = ?1 AND status = 'pending'",
                params![run_id, now],
            )?;
            Ok(count)
        })
    }

    /// Cancel any pending questions whose owning run is no longer `running`.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the update fails.
    pub fn cancel_orphaned_pending_questions(&self) -> Result<usize> {
        let now = now_secs();
        self.transaction(|tx| {
            let count = tx.execute(
                "UPDATE run_questions
                 SET status = 'cancelled', answered_at = ?1
                 WHERE status = 'pending'
                   AND run_id IN (SELECT id FROM agent_runs WHERE status != 'running')",
                params![now],
            )?;
            Ok(count)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::{configure, migrate};
    use crate::storage::repository::agent_runs::AgentRunRepository;
    use rusqlite::Connection;

    fn test_db() -> Database {
        let mut conn = Connection::open_in_memory().expect("open");
        configure(&conn).expect("configure");
        migrate(&mut conn).expect("migrate");
        Database::new(conn)
    }

    fn seed_conv_and_run(db: &Database) -> (i64, i64) {
        let now = now_secs();
        {
            let conn = db.lock().expect("lock");
            conn.execute(
                "INSERT INTO conversations (title, status, created_at, updated_at)
                 VALUES ('Q Test', 'active', ?1, ?1)",
                [now],
            )
            .expect("insert conv");
        }
        let conv_id = {
            let conn = db.lock().expect("lock");
            conn.last_insert_rowid()
        };
        let run_id = AgentRunRepository::new(db)
            .create_run(Some(conv_id), "gpt-test", "semi_autonomous")
            .expect("create run");
        (conv_id, run_id)
    }

    #[test]
    fn create_answer_and_cancel_roundtrip() {
        let db = test_db();
        let (conv_id, run_id) = seed_conv_and_run(&db);
        let repo = RunQuestionRepository::new(&db);

        let opts = vec!["JSON".to_string(), "YAML".to_string()];
        let created = repo
            .create_pending(NewRunQuestion {
                run_id,
                conversation_id: conv_id,
                call_id: "call_1",
                question: "Which format?",
                options: &opts,
                multi_select: false,
                allow_free_text: true,
            })
            .expect("create");
        assert_eq!(created.status, "pending");
        assert_eq!(created.options, vec!["JSON", "YAML"]);

        let pending = repo
            .list_pending_for_conversation(conv_id)
            .expect("list pending");
        assert_eq!(pending.len(), 1);

        let answered = repo
            .answer_question(created.id, &["JSON".into()], Some("with comments"))
            .expect("answer")
            .expect("updated");
        assert_eq!(answered.status, "answered");
        assert_eq!(answered.selected_options, vec!["JSON"]);
        assert_eq!(answered.free_text.as_deref(), Some("with comments"));

        assert!(repo
            .answer_question(created.id, &["YAML".into()], None)
            .expect("re-answer")
            .is_none());

        let second = repo
            .create_pending(NewRunQuestion {
                run_id,
                conversation_id: conv_id,
                call_id: "call_2",
                question: "Continue?",
                options: &[],
                multi_select: false,
                allow_free_text: true,
            })
            .expect("create 2");
        assert_eq!(repo.cancel_pending_for_run(run_id).expect("cancel"), 1);
        let reloaded = repo.read_by_id(second.id).expect("read").expect("exists");
        assert_eq!(reloaded.status, "cancelled");
    }
}
