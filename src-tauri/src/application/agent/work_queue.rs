//! Persistent per-conversation FIFO work queue service (Stage 6 PR 6A).

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::work_queue::{WorkQueueItem, WorkQueueRepository};

/// Classified application errors for work queue operations.
#[derive(Debug)]
pub(crate) enum WorkQueueError {
    InvalidInput(&'static str),
    NotFound(&'static str),
    Database(DatabaseError),
}

impl From<DatabaseError> for WorkQueueError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

/// Enqueue a prompt to the FIFO tail of `conversation_id`.
pub(crate) fn enqueue(
    db: &Database,
    conversation_id: i64,
    content: &str,
    provider: &str,
    model: &str,
    effort: Option<&str>,
) -> Result<WorkQueueItem, WorkQueueError> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(WorkQueueError::InvalidInput(
            "the queued prompt must not be empty",
        ));
    }
    if provider.trim().is_empty() || model.trim().is_empty() {
        return Err(WorkQueueError::InvalidInput(
            "a provider and model are required",
        ));
    }
    let exists = ConversationRepository::new(db)
        .read(conversation_id)?
        .is_some();
    if !exists {
        return Err(WorkQueueError::NotFound("conversation not found"));
    }
    Ok(WorkQueueRepository::new(db).enqueue(
        conversation_id,
        trimmed,
        provider.trim(),
        model.trim(),
        effort.map(str::trim).filter(|value| !value.is_empty()),
    )?)
}

/// List all queued items for `conversation_id` in FIFO order.
pub(crate) fn list(
    db: &Database,
    conversation_id: i64,
) -> Result<Vec<WorkQueueItem>, WorkQueueError> {
    Ok(WorkQueueRepository::new(db).list_queued(conversation_id)?)
}

/// Update the prompt text of a queued item.
pub(crate) fn update_content(
    db: &Database,
    item_id: i64,
    content: &str,
) -> Result<WorkQueueItem, WorkQueueError> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(WorkQueueError::InvalidInput(
            "the queued prompt must not be empty",
        ));
    }
    WorkQueueRepository::new(db)
        .update_content(item_id, trimmed)?
        .ok_or(WorkQueueError::NotFound("queued item not found"))
}

/// Reorder queued items for `conversation_id` to match `ordered_ids`.
pub(crate) fn reorder(
    db: &Database,
    conversation_id: i64,
    ordered_ids: &[i64],
) -> Result<Vec<WorkQueueItem>, WorkQueueError> {
    Ok(WorkQueueRepository::new(db).reorder(conversation_id, ordered_ids)?)
}

/// Cancel (remove from queue) a queued item and return its owning `conversation_id`.
pub(crate) fn cancel_item(db: &Database, item_id: i64) -> Result<i64, WorkQueueError> {
    let repo = WorkQueueRepository::new(db);
    let item = repo
        .read_by_id(item_id)?
        .ok_or(WorkQueueError::NotFound("queued item not found"))?;
    if repo.cancel_item(item_id)? {
        Ok(item.conversation_id)
    } else {
        Err(WorkQueueError::NotFound("queued item not found"))
    }
}

/// Atomically claim the next FIFO `queued` item (`status = 'running'`) for `conversation_id`.
pub(crate) fn claim_next(
    db: &Database,
    conversation_id: i64,
) -> Result<Option<WorkQueueItem>, WorkQueueError> {
    Ok(WorkQueueRepository::new(db).claim_next_queued(conversation_id)?)
}

/// Finalize a claimed `'running'` item to `'completed'` or `'cancelled'`.
pub(crate) fn finish_claimed(
    db: &Database,
    item_id: i64,
    completed: bool,
) -> Result<bool, WorkQueueError> {
    Ok(WorkQueueRepository::new(db).finish_claimed(item_id, completed)?)
}

/// Restore a claimed `'running'` item back to `'queued'` if starting the run failed.
pub(crate) fn revert_claim(db: &Database, item_id: i64) -> Result<bool, WorkQueueError> {
    Ok(WorkQueueRepository::new(db).revert_claim(item_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    #[test]
    fn enqueue_reorder_edit_and_cancel_fifo_items() {
        let db = in_memory_database();
        let conv_id = ConversationRepository::new(&db)
            .create("Queue Conv", "active")
            .expect("create conversation");

        assert!(matches!(
            enqueue(&db, conv_id, "   ", "openai", "gpt-5", None),
            Err(WorkQueueError::InvalidInput(_))
        ));
        assert!(matches!(
            enqueue(&db, 99999, "hello", "openai", "gpt-5", None),
            Err(WorkQueueError::NotFound(_))
        ));

        let first = enqueue(&db, conv_id, "First task", "openai", "gpt-5", Some("high"))
            .expect("enqueue 1");
        let second =
            enqueue(&db, conv_id, "Second task", "openai", "gpt-5", None).expect("enqueue 2");

        let listed = list(&db, conv_id).expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, first.id);
        assert_eq!(listed[1].id, second.id);

        let edited = update_content(&db, first.id, "First edited").expect("edit");
        assert_eq!(edited.content, "First edited");

        let reordered = reorder(&db, conv_id, &[second.id, first.id]).expect("reorder");
        assert_eq!(reordered[0].id, second.id);
        assert_eq!(reordered[1].id, first.id);

        assert_eq!(cancel_item(&db, second.id).expect("cancel"), conv_id);
        assert!(matches!(
            cancel_item(&db, second.id),
            Err(WorkQueueError::NotFound(_))
        ));
        let remaining = list(&db, conv_id).expect("list after cancel");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, first.id);

        let claimed = claim_next(&db, conv_id)
            .expect("claim next")
            .expect("claimed item");
        assert_eq!(claimed.id, first.id);
        assert_eq!(claimed.status, "running");
        assert!(revert_claim(&db, claimed.id).expect("revert claim"));
        assert_eq!(list(&db, conv_id).expect("list after revert").len(), 1);

        let reclaimed = claim_next(&db, conv_id)
            .expect("reclaim")
            .expect("reclaimed item");
        assert!(finish_claimed(&db, reclaimed.id, true).expect("finish claimed"));
        assert!(list(&db, conv_id).expect("list after finish").is_empty());
        assert!(claim_next(&db, conv_id).expect("empty claim").is_none());
    }
}
