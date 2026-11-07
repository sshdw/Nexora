//! Task CRUD service: validation plus orchestration over the settings store.
//!
//! Split from `tasks.rs` (AC-207): [`TaskService`] (v8 CHECK mirroring,
//! create/update/delete, lock acquire/heartbeat/release/move) lives here.

use std::time::{SystemTime, UNIX_EPOCH};

use super::registry::TaskRegistry;
use super::{
    TaskError, KANBAN_MOVE_STATUSES, STEP_TITLE_MAX_LEN, TASK_DESCRIPTION_MAX_LEN,
    TASK_LOCK_HOLDER_MAX_LEN, TASK_LOCK_TTL_MAX_SECS, TASK_LOCK_TTL_MIN_SECS, TASK_LOCK_TTL_SECS,
    TASK_MAX_STEPS, TASK_TITLE_MAX_LEN,
};

use crate::application::conversations::{ConversationError, ConversationService};
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::agent_tasks::{
    AgentTask, AgentTaskRepository, AgentTaskStep, LockAcquireOutcome, TaskLock,
};

// ---------------------------------------------------------------------------
// Service (validation + CRUD orchestration)
// ---------------------------------------------------------------------------

/// Task CRUD over the shared [`Database`]. Validation mirrors the v8 CHECKs
/// so invalid input fails here with fixed vocabulary instead of as raw
/// `SQLite` errors.
pub(crate) struct TaskService<'a> {
    db: &'a Database,
}

impl<'a> TaskService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Validate a task title (non-empty after trimming, within the bound).
    fn validate_title(title: &str) -> Result<String, TaskError> {
        let trimmed = title.trim().to_string();
        if trimmed.is_empty() || trimmed.chars().count() > TASK_TITLE_MAX_LEN {
            return Err(TaskError::InvalidInput {
                message: "the task title must be 1..200 characters".to_string(),
            });
        }
        Ok(trimmed)
    }

    /// Validate an optional description (empty maps to `None`).
    fn validate_description(description: Option<&str>) -> Result<Option<String>, TaskError> {
        let Some(text) = description else {
            return Ok(None);
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.chars().count() > TASK_DESCRIPTION_MAX_LEN {
            return Err(TaskError::InvalidInput {
                message: "the task description must be at most 4000 characters".to_string(),
            });
        }
        Ok(Some(trimmed.to_string()))
    }

    /// Validate the step list (1..=`TASK_MAX_STEPS` non-empty titles).
    fn validate_steps(steps: &[String]) -> Result<Vec<String>, TaskError> {
        let count = i64::try_from(steps.len()).unwrap_or(i64::MAX);
        if steps.is_empty() || count > TASK_MAX_STEPS {
            return Err(TaskError::InvalidInput {
                message: "the task must define 1..25 steps".to_string(),
            });
        }
        let mut clean = Vec::with_capacity(steps.len());
        for step in steps {
            let trimmed = step.trim().to_string();
            if trimmed.is_empty() || trimmed.chars().count() > STEP_TITLE_MAX_LEN {
                return Err(TaskError::InvalidInput {
                    message: "each step title must be 1..500 characters".to_string(),
                });
            }
            clean.push(trimmed);
        }
        Ok(clean)
    }

    /// Resolve the effective step cap: explicit `max_steps` must land in
    /// `1..=TASK_MAX_STEPS`; absent means one pass over the step list.
    fn resolve_max_steps(max_steps: Option<i64>, step_count: usize) -> Result<i64, TaskError> {
        match max_steps {
            None => Ok(i64::try_from(step_count).unwrap_or(TASK_MAX_STEPS)),
            Some(cap) if (1..=TASK_MAX_STEPS).contains(&cap) => Ok(cap),
            Some(_) => Err(TaskError::InvalidInput {
                message: "the step cap must be 1..25".to_string(),
            }),
        }
    }

    /// Create a task with its steps. When `conversation_id` is `None`, a
    /// backing conversation (`Task: <title>`) is created first so the
    /// autonomous loop's agent runs have a conversation to execute inside
    /// of. Returns the task `id` (steps are stored `seq` 1-based in order).
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::InvalidInput`] for out-of-bounds titles,
    /// descriptions, step lists, or caps, and [`TaskError::Database`] for
    /// persistence failures (including a missing explicit conversation).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_task(
        &self,
        title: &str,
        description: Option<&str>,
        conversation_id: Option<i64>,
        provider: Option<&str>,
        model: Option<&str>,
        steps: &[String],
        max_steps: Option<i64>,
    ) -> Result<i64, TaskError> {
        let title = Self::validate_title(title)?;
        let description = Self::validate_description(description)?;
        let steps = Self::validate_steps(steps)?;
        let cap = Self::resolve_max_steps(max_steps, steps.len())?;
        let provider = non_empty(provider);
        let model = non_empty(model);

        let conversation_id = if let Some(id) = conversation_id {
            Some(id)
        } else {
            let backing = format!("Task: {}", title.chars().take(100).collect::<String>());
            match ConversationService::new(self.db).create(&backing) {
                Ok(id) => Some(id),
                Err(ConversationError::Database(inner)) => {
                    return Err(TaskError::Database(inner));
                }
                Err(other) => {
                    log::warn!("task create: backing conversation failed: {other}");
                    return Err(TaskError::RunStart {
                        message: "the task conversation could not be created".to_string(),
                    });
                }
            }
        };

        let repo = AgentTaskRepository::new(self.db);
        let id = repo
            .create_task(
                &title,
                description.as_deref(),
                conversation_id,
                provider.as_deref(),
                model.as_deref(),
                cap,
            )
            .map_err(TaskError::Database)?;
        for (index, step) in steps.iter().enumerate() {
            let seq = i64::try_from(index + 1).unwrap_or(i64::MAX);
            repo.append_step(id, seq, step)
                .map_err(TaskError::Database)?;
        }
        Ok(id)
    }

    /// List all tasks, most recently active first.
    ///
    /// # Errors
    ///
    /// Propagates [`DatabaseError`].
    pub(crate) fn list_tasks(&self) -> Result<Vec<AgentTask>, DatabaseError> {
        AgentTaskRepository::new(self.db).list_tasks()
    }

    /// Read one task by `id`.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] when no task exists; propagates
    /// [`DatabaseError`] on read failure.
    pub(crate) fn read_task(&self, id: i64) -> Result<AgentTask, TaskError> {
        AgentTaskRepository::new(self.db)
            .read_task(id)
            .map_err(TaskError::Database)?
            .ok_or(TaskError::TaskNotFound { id })
    }

    /// List one task's steps, `seq` ascending.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] when no task exists; propagates
    /// [`DatabaseError`] on read failure.
    pub(crate) fn list_steps(&self, task_id: i64) -> Result<Vec<AgentTaskStep>, TaskError> {
        self.read_task(task_id)?;
        AgentTaskRepository::new(self.db)
            .list_steps(task_id)
            .map_err(TaskError::Database)
    }

    /// Update a pending task's title/description. Running tasks are rejected:
    /// edits mid-loop would race the report the loop composes.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] for unknown ids,
    /// [`TaskError::InvalidInput`] for out-of-bounds text or for edits to a
    /// running task; propagates [`DatabaseError`] on write failure.
    pub(crate) fn update_task(
        &self,
        id: i64,
        title: &str,
        description: Option<&str>,
    ) -> Result<(), TaskError> {
        let task = self.read_task(id)?;
        if task.status == "running" {
            return Err(TaskError::InvalidInput {
                message: "a running task cannot be edited".to_string(),
            });
        }
        let title = Self::validate_title(title)?;
        let description = Self::validate_description(description)?;
        AgentTaskRepository::new(self.db)
            .update_task(id, &title, description.as_deref())
            .map_err(TaskError::Database)
    }

    /// Delete a task (its steps cascade via the schema). Deleting a running
    /// task is rejected: stop it first so the loop thread never writes to a
    /// deleted row.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] for unknown ids,
    /// [`TaskError::InvalidInput`] for deletes of a running task; propagates
    /// [`DatabaseError`] on write failure.
    pub(crate) fn delete_task(&self, id: i64, registry: &TaskRegistry) -> Result<(), TaskError> {
        let task = self.read_task(id)?;
        if task.status == "running" || registry.is_active(id) {
            return Err(TaskError::InvalidInput {
                message: "stop the task before deleting it".to_string(),
            });
        }
        AgentTaskRepository::new(self.db)
            .delete_task(id)
            .map_err(TaskError::Database)
    }

    /// Validate a lock holder label (non-empty after trimming, within the
    /// v11 bound).
    fn validate_holder(holder: &str) -> Result<String, TaskError> {
        let trimmed = holder.trim().to_string();
        if trimmed.is_empty() || trimmed.chars().count() > TASK_LOCK_HOLDER_MAX_LEN {
            return Err(TaskError::InvalidInput {
                message: "the lock holder must be 1..64 characters".to_string(),
            });
        }
        Ok(trimmed)
    }

    /// Resolve the effective lock TTL: explicit values must land in
    /// `10..=600` seconds; absent means the 120s default. There is no path
    /// to a lock without a deadline.
    fn resolve_lock_ttl(ttl_secs: Option<i64>) -> Result<i64, TaskError> {
        match ttl_secs {
            None => Ok(TASK_LOCK_TTL_SECS),
            Some(ttl) if (TASK_LOCK_TTL_MIN_SECS..=TASK_LOCK_TTL_MAX_SECS).contains(&ttl) => {
                Ok(ttl)
            }
            Some(_) => Err(TaskError::InvalidInput {
                message: "the lock TTL must be 10..600 seconds".to_string(),
            }),
        }
    }

    /// Acquire the kanban edit lock for `task_id` with a deadline of
    /// `now + ttl`. Returns the lock row (its token is the proof a later
    /// `move_task` / `release_lock` / `heartbeat_lock` must present).
    ///
    /// A live lock by another holder is denied with [`TaskError::LockHeld`]
    /// (holder + deadline attached); an expired row is stolen.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] for unknown ids,
    /// [`TaskError::InvalidInput`] for out-of-bounds holders or TTLs, and
    /// [`TaskError::LockHeld`] on contention; propagates [`DatabaseError`]
    /// on write failure.
    pub(crate) fn acquire_lock(
        &self,
        task_id: i64,
        holder: &str,
        ttl_secs: Option<i64>,
    ) -> Result<TaskLock, TaskError> {
        self.read_task(task_id)?;
        let holder = Self::validate_holder(holder)?;
        let ttl = Self::resolve_lock_ttl(ttl_secs)?;
        let now = now_secs();
        let token = mint_lock_token(task_id, &holder, now);
        match AgentTaskRepository::new(self.db)
            .acquire_task_lock(task_id, &holder, &token, now, now + ttl)
            .map_err(TaskError::Database)?
        {
            LockAcquireOutcome::Acquired(lock) => Ok(lock),
            LockAcquireOutcome::Denied(live) => Err(TaskError::LockHeld {
                task_id,
                holder: live.holder,
                expires_at: live.expires_at,
            }),
        }
    }

    /// Release the edit lock for `task_id`. The presented token must match
    /// the stored row exactly; a missing row or a mismatch fails with
    /// [`TaskError::InvalidLock`] (fixed vocabulary — the stored token is
    /// never echoed).
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] for unknown ids and
    /// [`TaskError::InvalidLock`] for a missing/mismatched lock; propagates
    /// [`DatabaseError`] on write failure.
    pub(crate) fn release_lock(&self, task_id: i64, token: &str) -> Result<(), TaskError> {
        self.read_task(task_id)?;
        let repo = AgentTaskRepository::new(self.db);
        let Some(lock) = repo.read_lock(task_id).map_err(TaskError::Database)? else {
            return Err(TaskError::InvalidLock {
                message: "the task has no active lock".to_string(),
            });
        };
        if lock.token != token {
            return Err(TaskError::InvalidLock {
                message: "the lock token does not match".to_string(),
            });
        }
        repo.delete_lock(task_id).map_err(TaskError::Database)
    }

    /// Extend a live lock's deadline by a fresh TTL (heartbeat). Only the
    /// matching, still-valid token extends; a missing row, a mismatch, or an
    /// already-expired row fails — heartbeats never resurrect dead locks.
    /// Returns the refreshed row.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] for unknown ids,
    /// [`TaskError::InvalidInput`] for out-of-bounds TTLs, and
    /// [`TaskError::InvalidLock`] when there is nothing live to extend;
    /// propagates [`DatabaseError`] on write failure.
    pub(crate) fn heartbeat_lock(
        &self,
        task_id: i64,
        token: &str,
        ttl_secs: Option<i64>,
    ) -> Result<TaskLock, TaskError> {
        self.read_task(task_id)?;
        let ttl = Self::resolve_lock_ttl(ttl_secs)?;
        let now = now_secs();
        let repo = AgentTaskRepository::new(self.db);
        let extended = repo
            .extend_lock_expiry(task_id, token, now, now + ttl)
            .map_err(TaskError::Database)?;
        if !extended {
            return Err(TaskError::InvalidLock {
                message: "the lock is missing, mismatched, or expired".to_string(),
            });
        }
        repo.read_lock(task_id)
            .map_err(TaskError::Database)?
            .ok_or(TaskError::InvalidLock {
                message: "the lock is missing, mismatched, or expired".to_string(),
            })
    }

    /// Move a task to a kanban status, guarded by a live lock token. The
    /// token must match the stored row and be unexpired; stale, mismatched,
    /// or missing locks fail with [`TaskError::InvalidLock`] and change
    /// nothing. `'running'` is never a legal move target (the autonomous
    /// loop owns it — start runs through `start_task_run`), and a task the
    /// loop currently owns cannot be moved at all.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] for unknown ids,
    /// [`TaskError::InvalidInput`] for an illegal status or for a task the
    /// loop owns, and [`TaskError::InvalidLock`] for a missing/stale token;
    /// propagates [`DatabaseError`] on write failure.
    pub(crate) fn move_task(
        &self,
        task_id: i64,
        status: &str,
        token: &str,
    ) -> Result<(), TaskError> {
        if !KANBAN_MOVE_STATUSES.contains(&status) {
            return Err(TaskError::InvalidInput {
                message: "the kanban status must be pending, completed, failed, or cancelled"
                    .to_string(),
            });
        }
        let task = self.read_task(task_id)?;
        if task.status == "running" {
            return Err(TaskError::InvalidInput {
                message: "a running task is owned by the autonomous loop — stop it first"
                    .to_string(),
            });
        }
        let now = now_secs();
        let repo = AgentTaskRepository::new(self.db);
        let Some(lock) = repo.read_lock(task_id).map_err(TaskError::Database)? else {
            return Err(TaskError::InvalidLock {
                message: "the task has no active lock — acquire one before moving".to_string(),
            });
        };
        if lock.token != token || lock.expires_at <= now {
            return Err(TaskError::InvalidLock {
                message: "the lock token is stale or expired — re-acquire the lock".to_string(),
            });
        }
        repo.set_task_status(task_id, status)
            .map_err(TaskError::Database)
    }
}

/// Map empty/whitespace-only optionals to `None` (an explicit empty provider
/// is "unset", never a provider named `""`).
fn non_empty(value: Option<&str>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

/// Current time as Unix seconds (lock deadlines ride wall-clock seconds,
/// matching the `unixepoch()` the schema defaults use).
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Mint an opaque lock token. No `rand`/`uuid` dependency exists in this
/// crate, so the token hashes the task, the holder, wall-clock seconds, and
/// sub-second nanos plus the process id — unique per acquisition in practice,
/// and uniqueness is additionally enforced by the v11 `UNIQUE(token)`
/// constraint (a collision surfaces as a retryable database error, never a
/// silent alias).
fn mint_lock_token(task_id: i64, holder: &str, now: i64) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let mut hasher = DefaultHasher::new();
    task_id.hash(&mut hasher);
    holder.hash(&mut hasher);
    now.hash(&mut hasher);
    nanos.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    format!("{:016x}-{:08x}", hasher.finish(), nanos)
}
