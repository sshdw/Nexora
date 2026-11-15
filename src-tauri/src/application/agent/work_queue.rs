//! Persistent per-conversation FIFO work queue service (Stage 6 PR 6A).

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::work_queue::{WorkQueueItem, WorkQueueRepository};

/// Classified application errors for work queue operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkQueueError {
    InvalidInput(&'static str),
    NotFound(&'static str),
    Database(String),
}

impl From<DatabaseError> for WorkQueueError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err.to_string())
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

/// Cancel (remove from queue) a queued item.
pub(crate) fn cancel_item(db: &Database, item_id: i64) -> Result<(), WorkQueueError> {
    if WorkQueueRepository::new(db).cancel_item(item_id)? {
        Ok(())
    } else {
        Err(WorkQueueError::NotFound("queued item not found"))
    }
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

        cancel_item(&db, second.id).expect("cancel");
        let remaining = list(&db, conv_id).expect("list after cancel");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, first.id);
    }
}
