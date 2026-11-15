//! Durable per-conversation FIFO work queue repository (`work_queue_items`, v18).

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use crate::storage::database::{now_secs, Database};
use crate::storage::repository::{Repository, Result};

/// Persisted `work_queue_items` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkQueueItem {
    /// Primary key of the queued item.
    pub id: i64,
    /// Owning conversation identifier.
    pub conversation_id: i64,
    /// Queued user prompt content.
    pub content: String,
    /// Provider name captured when queued.
    pub provider: String,
    /// Model name captured when queued.
    pub model: String,
    /// Optional reasoning effort captured when queued.
    pub effort: Option<String>,
    /// Zero-based FIFO ordering position among queued items.
    pub position: i64,
    /// Queue item lifecycle status (`queued`, `running`, `completed`, `cancelled`).
    pub status: String,
    /// Unix timestamp (seconds) when queued.
    pub created_at: i64,
    /// Unix timestamp (seconds) of the last update.
    pub updated_at: i64,
}

const SELECT_COLUMNS: &str =
    "id, conversation_id, content, provider, model, effort, position, status, created_at, updated_at";

fn map_item(row: &Row<'_>) -> rusqlite::Result<WorkQueueItem> {
    Ok(WorkQueueItem {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        content: row.get(2)?,
        provider: row.get(3)?,
        model: row.get(4)?,
        effort: row.get(5)?,
        position: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// Persistence adapter over `work_queue_items`.
pub struct WorkQueueRepository<'a> {
    db: &'a Database,
}

impl Repository for WorkQueueRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl<'a> WorkQueueRepository<'a> {
    /// Construct a repository bound to `db`.
    #[must_use]
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Append a queued item to the FIFO tail for `conversation_id`.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) on `SQLite` errors or constraint violations.
    pub fn enqueue(
        &self,
        conversation_id: i64,
        content: &str,
        provider: &str,
        model: &str,
        effort: Option<&str>,
    ) -> Result<WorkQueueItem> {
        let now = now_secs();
        let trimmed = content.trim();
        let id = self.transaction(|tx| {
            let next_pos: i64 = tx.query_row(
                "SELECT COALESCE(MAX(position) + 1, 0)
                 FROM work_queue_items
                 WHERE conversation_id = ?1 AND status = 'queued'",
                [conversation_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO work_queue_items (
                    conversation_id, content, provider, model, effort, position, status, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?7)",
                params![conversation_id, trimmed, provider, model, effort, next_pos, now],
            )?;
            Ok(tx.last_insert_rowid())
        })?;
        self.read_by_id(id)?.ok_or_else(|| {
            crate::storage::database::DatabaseError::Sqlite(rusqlite::Error::QueryReturnedNoRows)
        })
    }

    /// Read one queue item by primary key.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the query fails.
    pub fn read_by_id(&self, id: i64) -> Result<Option<WorkQueueItem>> {
        let conn = self.read_conn()?;
        let sql = format!("SELECT {SELECT_COLUMNS} FROM work_queue_items WHERE id = ?1");
        conn.query_row(&sql, [id], map_item)
            .optional()
            .map_err(Into::into)
    }

    /// List all pending (`queued`) items for a conversation in FIFO order (`position ASC, id ASC`).
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the query fails.
    pub fn list_queued(&self, conversation_id: i64) -> Result<Vec<WorkQueueItem>> {
        let conn = self.read_conn()?;
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM work_queue_items
             WHERE conversation_id = ?1 AND status = 'queued'
             ORDER BY position ASC, id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([conversation_id], map_item)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Update the text content of a `queued` item. Returns `Ok(Some(item))` if updated.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the update fails.
    pub fn update_content(&self, item_id: i64, content: &str) -> Result<Option<WorkQueueItem>> {
        let now = now_secs();
        let trimmed = content.trim();
        let changed = self.transaction(|tx| {
            let count = tx.execute(
                "UPDATE work_queue_items
                 SET content = ?2, updated_at = ?3
                 WHERE id = ?1 AND status = 'queued'",
                params![item_id, trimmed, now],
            )?;
            Ok(count)
        })?;
        if changed == 0 {
            return Ok(None);
        }
        self.read_by_id(item_id)
    }

    /// Reorder queued items for `conversation_id` to match `ordered_ids` (any unmentioned
    /// queued items keep their relative order after the given ones).
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the update fails.
    pub fn reorder(&self, conversation_id: i64, ordered_ids: &[i64]) -> Result<Vec<WorkQueueItem>> {
        let now = now_secs();
        self.transaction(|tx| {
            let mut stmt = tx.prepare(
                "SELECT id FROM work_queue_items
                 WHERE conversation_id = ?1 AND status = 'queued'
                 ORDER BY position ASC, id ASC",
            )?;
            let existing: Vec<i64> = stmt
                .query_map([conversation_id], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);

            let mut final_order: Vec<i64> = Vec::with_capacity(existing.len());
            for id in ordered_ids {
                if existing.contains(id) && !final_order.contains(id) {
                    final_order.push(*id);
                }
            }
            for id in existing {
                if !final_order.contains(&id) {
                    final_order.push(id);
                }
            }

            for (idx, id) in final_order.into_iter().enumerate() {
                let pos = i64::try_from(idx).unwrap_or(i64::MAX);
                tx.execute(
                    "UPDATE work_queue_items
                     SET position = ?2, updated_at = ?3
                     WHERE id = ?1 AND conversation_id = ?4 AND status = 'queued'",
                    params![id, pos, now, conversation_id],
                )?;
            }
            Ok(())
        })?;
        self.list_queued(conversation_id)
    }

    /// Cancel a `queued` item (marks `status = 'cancelled'`). Returns `true` if updated.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the update fails.
    pub fn cancel_item(&self, item_id: i64) -> Result<bool> {
        let now = now_secs();
        self.transaction(|tx| {
            let count = tx.execute(
                "UPDATE work_queue_items
                 SET status = 'cancelled', updated_at = ?2
                 WHERE id = ?1 AND status = 'queued'",
                params![item_id, now],
            )?;
            Ok(count > 0)
        })
    }

    /// Atomically pop the next FIFO `queued` item into `'running'` for auto-advance.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the query or update fails.
    pub fn claim_next_queued(&self, conversation_id: i64) -> Result<Option<WorkQueueItem>> {
        let now = now_secs();
        let claimed_id = self.transaction(|tx| {
            let next_id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM work_queue_items
                     WHERE conversation_id = ?1 AND status = 'queued'
                     ORDER BY position ASC, id ASC
                     LIMIT 1",
                    [conversation_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(id) = next_id else {
                return Ok(None);
            };
            let count = tx.execute(
                "UPDATE work_queue_items
                 SET status = 'running', updated_at = ?2
                 WHERE id = ?1 AND status = 'queued'",
                params![id, now],
            )?;
            if count == 0 {
                return Ok(None);
            }
            Ok(Some(id))
        })?;
        match claimed_id {
            Some(id) => self.read_by_id(id),
            None => Ok(None),
        }
    }

    /// Finalize a claimed `'running'` item to `'completed'` or `'cancelled'`.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the update fails.
    pub fn finish_claimed(&self, item_id: i64, completed: bool) -> Result<bool> {
        let now = now_secs();
        let target_status = if completed { "completed" } else { "cancelled" };
        self.transaction(|tx| {
            let count = tx.execute(
                "UPDATE work_queue_items
                 SET status = ?2, updated_at = ?3
                 WHERE id = ?1 AND status = 'running'",
                params![item_id, target_status, now],
            )?;
            Ok(count > 0)
        })
    }

    /// Restore a claimed `'running'` item back to `'queued'` if starting the run failed.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the update fails.
    pub fn revert_claim(&self, item_id: i64) -> Result<bool> {
        let now = now_secs();
        self.transaction(|tx| {
            let count = tx.execute(
                "UPDATE work_queue_items
                 SET status = 'queued', updated_at = ?2
                 WHERE id = ?1 AND status = 'running'",
                params![item_id, now],
            )?;
            Ok(count > 0)
        })
    }

    /// Mark any stale `'running'` work queue items as `'cancelled'` on orphan sweep.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError`](crate::storage::database::DatabaseError) when the update fails.
    pub fn cancel_orphaned_running_items(&self) -> Result<usize> {
        let now = now_secs();
        self.transaction(|tx| {
            let count = tx.execute(
                "UPDATE work_queue_items
                 SET status = 'cancelled', updated_at = ?1
                 WHERE status = 'running'",
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
    use rusqlite::Connection;

    fn test_db() -> Database {
        let mut conn = Connection::open_in_memory().expect("open");
        configure(&conn).expect("configure");
        migrate(&mut conn).expect("migrate");
        Database::new(conn)
    }

    fn seed_conv(db: &Database) -> i64 {
        let now = now_secs();
        let conn = db.lock().expect("lock");
        conn.execute(
            "INSERT INTO conversations (title, status, created_at, updated_at)
             VALUES ('Queue Test', 'active', ?1, ?1)",
            [now],
        )
        .expect("insert conv");
        conn.last_insert_rowid()
    }

    #[test]
    fn enqueue_reorder_edit_claim_and_cancel_work_fifo() {
        let db = test_db();
        let conv_id = seed_conv(&db);
        let repo = WorkQueueRepository::new(&db);

        let a = repo
            .enqueue(conv_id, "first task", "openai", "gpt-4o", Some("medium"))
            .expect("enqueue a");
        let b = repo
            .enqueue(conv_id, "second task", "openai", "gpt-4o", None)
            .expect("enqueue b");
        let c = repo
            .enqueue(conv_id, "third task", "openai", "gpt-4o", None)
            .expect("enqueue c");

        let list = repo.list_queued(conv_id).expect("list");
        assert_eq!(
            list.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![a.id, b.id, c.id]
        );

        let reordered = repo.reorder(conv_id, &[b.id, a.id, c.id]).expect("reorder");
        assert_eq!(
            reordered.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![b.id, a.id, c.id]
        );

        let edited = repo
            .update_content(b.id, "updated second task")
            .expect("update")
            .expect("present");
        assert_eq!(edited.content, "updated second task");

        assert!(repo.cancel_item(c.id).expect("cancel c"));
        let remaining = repo.list_queued(conv_id).expect("list 2");
        assert_eq!(remaining.len(), 2);

        let claimed = repo
            .claim_next_queued(conv_id)
            .expect("claim")
            .expect("item");
        assert_eq!(claimed.id, b.id);
        assert_eq!(claimed.status, "running");
        assert!(repo.finish_claimed(claimed.id, true).expect("finish"));
        let after = repo.list_queued(conv_id).expect("list 3");
        assert_eq!(after.iter().map(|i| i.id).collect::<Vec<_>>(), vec![a.id]);
    }
}
