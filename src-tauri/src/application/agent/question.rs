//! Per-run live clarifying question gate (`ask_user_question`, Stage 6 PR 6A).
//!
//! Parks only the owning run thread cleanly on a `Condvar` + `CancellationToken`
//! until the user answers via [`QuestionGate::resolve`] or cancels/shuts down the run.
//! Other conversations and Split panes remain completely unblocked.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::run_questions::{
    NewRunQuestion, RunQuestion, RunQuestionRepository,
};

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(200);

fn default_allow_free_text() -> bool {
    true
}

/// Parsed arguments for the `ask_user_question` tool call.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AskQuestionArgs {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default = "default_allow_free_text")]
    pub allow_free_text: bool,
}

impl AskQuestionArgs {
    /// Parse and validate tool call JSON arguments.
    pub(crate) fn parse(raw_json: &str) -> Result<Self, String> {
        let mut parsed: Self = serde_json::from_str(raw_json)
            .map_err(|err| format!("invalid ask_user_question arguments: {err}"))?;
        parsed.question = parsed.question.trim().to_string();
        if parsed.question.is_empty() {
            return Err("field 'question' must not be empty".to_string());
        }
        parsed.options = parsed
            .options
            .into_iter()
            .map(|opt| opt.trim().to_string())
            .filter(|opt| !opt.is_empty())
            .collect();
        if !parsed.allow_free_text && parsed.options.is_empty() {
            return Err(
                "at least one option is required when 'allow_free_text' is false".to_string(),
            );
        }
        Ok(parsed)
    }
}

/// Live snapshot of a parked `ask_user_question` prompt, serialized over IPC
/// and `agent-run-event` in `snake_case`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PendingQuestionSnapshot {
    pub question_id: i64,
    pub run_id: i64,
    pub conversation_id: i64,
    pub call_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub multi_select: bool,
    pub allow_free_text: bool,
    pub created_at: i64,
}

impl PendingQuestionSnapshot {
    fn from_row(row: &RunQuestion) -> Self {
        Self {
            question_id: row.id,
            run_id: row.run_id,
            conversation_id: row.conversation_id,
            call_id: row.call_id.clone(),
            question: row.question.clone(),
            options: row.options.clone(),
            multi_select: row.multi_select,
            allow_free_text: row.allow_free_text,
            created_at: row.created_at,
        }
    }
}

/// User-submitted answer delivered back to the parked LLM tool turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct QuestionAnswer {
    pub selected_options: Vec<String>,
    pub free_text: Option<String>,
}

/// Reason `ask_and_wait` exited without an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QuestionWaitEnd {
    Cancelled,
    Database(String),
}

/// Validation / state errors when resolving a pending question via IPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QuestionResolveError {
    RunNotActive,
    NotPending,
    InvalidAnswer(String),
    Database(String),
}

#[derive(Debug, Default)]
struct QuestionGateState {
    pending: Option<PendingQuestionSnapshot>,
    answer: Option<QuestionAnswer>,
}

/// Per-run context passed into [`AgentRunner`](super::runner::AgentRunner)
/// so `ask_user_question` can persist rows and park on [`QuestionGate`].
#[derive(Clone)]
pub(crate) struct QuestionRunContext {
    pub db: Database,
    pub run_id: i64,
    pub conversation_id: i64,
    pub gate: QuestionGate,
}

impl std::fmt::Debug for QuestionRunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionRunContext")
            .field("run_id", &self.run_id)
            .field("conversation_id", &self.conversation_id)
            .finish_non_exhaustive()
    }
}

/// Cancellation-safe per-run waiter for `ask_user_question`.
#[derive(Debug, Clone)]
pub(crate) struct QuestionGate {
    token: CancellationToken,
    state: Arc<Mutex<QuestionGateState>>,
    signal: Arc<Condvar>,
}

impl QuestionGate {
    /// Construct a gate directly from a shared [`CancellationToken`].
    #[must_use]
    pub(crate) fn with_token(token: CancellationToken) -> Self {
        Self {
            token,
            state: Arc::new(Mutex::new(QuestionGateState::default())),
            signal: Arc::new(Condvar::new()),
        }
    }

    /// Construct a gate linked to `control`'s cancellation token and wakeup list.
    #[must_use]
    pub(crate) fn new(control: &RunControl) -> Self {
        let signal = Arc::new(Condvar::new());
        control.register_waiter(Arc::downgrade(&signal));
        Self {
            token: control.token().clone(),
            state: Arc::new(Mutex::new(QuestionGateState::default())),
            signal,
        }
    }

    /// Wake any parked `ask_and_wait` caller (used by [`RunControl`] on cancel/shutdown).
    pub(crate) fn notify(&self) {
        self.signal.notify_all();
    }

    fn lock_state(&self) -> MutexGuard<'_, QuestionGateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Snapshot of the currently parked question, if any.
    #[must_use]
    pub(crate) fn pending_snapshot(&self) -> Option<PendingQuestionSnapshot> {
        self.lock_state().pending.clone()
    }

    /// Persist a pending question, emit `QuestionRequested`, and park only
    /// this run until answered or cancelled.
    pub(crate) fn ask_and_wait(
        &self,
        db: &Database,
        run_id: i64,
        conversation_id: i64,
        call_id: &str,
        args: &AskQuestionArgs,
        sender: Option<&Sender<AgentRunEvent>>,
    ) -> Result<QuestionAnswer, QuestionWaitEnd> {
        if self.token.is_cancelled() {
            return Err(QuestionWaitEnd::Cancelled);
        }

        let created = RunQuestionRepository::new(db)
            .create_pending(NewRunQuestion {
                run_id,
                conversation_id,
                call_id,
                question: &args.question,
                options: &args.options,
                multi_select: args.multi_select,
                allow_free_text: args.allow_free_text,
            })
            .map_err(|err| QuestionWaitEnd::Database(err.to_string()))?;

        let snapshot = PendingQuestionSnapshot::from_row(&created);
        let question_id = snapshot.question_id;
        {
            let mut guard = self.lock_state();
            guard.pending = Some(snapshot.clone());
            guard.answer = None;
        }

        if let Some(tx) = sender {
            let _ = tx.send(AgentRunEvent::QuestionRequested { question: snapshot });
        }

        let mut guard = self.lock_state();
        loop {
            if let Some(answer) = guard.answer.take() {
                guard.pending = None;
                drop(guard);
                if let Some(tx) = sender {
                    let _ = tx.send(AgentRunEvent::QuestionResolved {
                        question_id,
                        call_id: call_id.to_string(),
                        status: "answered".to_string(),
                    });
                }
                return Ok(answer);
            }
            if self.token.is_cancelled() {
                guard.pending = None;
                drop(guard);
                let _ = RunQuestionRepository::new(db).cancel_pending_for_run(run_id);
                if let Some(tx) = sender {
                    let _ = tx.send(AgentRunEvent::QuestionResolved {
                        question_id,
                        call_id: call_id.to_string(),
                        status: "cancelled".to_string(),
                    });
                }
                return Err(QuestionWaitEnd::Cancelled);
            }
            let (next_guard, _) = self
                .signal
                .wait_timeout(guard, WAIT_POLL_INTERVAL)
                .unwrap_or_else(PoisonError::into_inner);
            guard = next_guard;
        }
    }

    /// Validate, persist, and unblock a parked question with the user's answer.
    pub(crate) fn resolve(
        &self,
        db: &Database,
        question_id: i64,
        selected_options: Vec<String>,
        free_text: Option<String>,
    ) -> Result<RunQuestion, QuestionResolveError> {
        let pending = {
            let guard = self.lock_state();
            guard
                .pending
                .clone()
                .filter(|item| item.question_id == question_id)
                .ok_or(QuestionResolveError::NotPending)?
        };

        let mut deduped_selected = Vec::new();
        for raw in selected_options {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            if !pending.options.is_empty() && !pending.options.contains(&trimmed) {
                return Err(QuestionResolveError::InvalidAnswer(
                    "selected option is not one of the available choices".to_string(),
                ));
            }
            if !deduped_selected.contains(&trimmed) {
                deduped_selected.push(trimmed);
            }
        }
        if !pending.multi_select && deduped_selected.len() > 1 {
            return Err(QuestionResolveError::InvalidAnswer(
                "only one option may be selected for a single-select question".to_string(),
            ));
        }
        let normalized_free = free_text
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        if !pending.allow_free_text && normalized_free.is_some() {
            return Err(QuestionResolveError::InvalidAnswer(
                "free-text input is not allowed for this question".to_string(),
            ));
        }
        if deduped_selected.is_empty() && normalized_free.is_none() {
            return Err(QuestionResolveError::InvalidAnswer(
                "select an option or enter a response before submitting".to_string(),
            ));
        }

        let updated = RunQuestionRepository::new(db)
            .answer_question(question_id, &deduped_selected, normalized_free.as_deref())
            .map_err(|err| QuestionResolveError::Database(err.to_string()))?
            .ok_or(QuestionResolveError::NotPending)?;

        {
            let mut guard = self.lock_state();
            if guard
                .pending
                .as_ref()
                .is_some_and(|item| item.question_id == question_id)
            {
                guard.pending = None;
                guard.answer = Some(QuestionAnswer {
                    selected_options: deduped_selected,
                    free_text: normalized_free,
                });
            }
        }
        self.signal.notify_all();
        Ok(updated)
    }
}

/// Application helper: list all questions (newest/created order) for a conversation.
pub(crate) fn list_for_conversation(
    db: &Database,
    conversation_id: i64,
) -> Result<Vec<RunQuestion>, DatabaseError> {
    RunQuestionRepository::new(db).list_for_conversation(conversation_id)
}

/// Application helper: list currently pending questions whose owning run is still running.
pub(crate) fn list_pending_for_conversation(
    db: &Database,
    conversation_id: i64,
) -> Result<Vec<RunQuestion>, DatabaseError> {
    RunQuestionRepository::new(db).list_pending_for_conversation(conversation_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;
    use crate::infrastructure::repository::conversations::ConversationRepository;
    use std::sync::mpsc::channel;
    use std::thread;

    fn seed_conv_and_run(db: &Database) -> (i64, i64) {
        let conv_id = ConversationRepository::new(db)
            .create("Question Test", "active")
            .expect("create conversation");
        let run_id = AgentRunRepository::new(db)
            .create_run(Some(conv_id), "gpt-test", "supervised")
            .expect("create run");
        (conv_id, run_id)
    }

    #[test]
    fn ask_and_wait_parks_then_resolves_and_persists() {
        let db = in_memory_database();
        let (conv_id, run_id) = seed_conv_and_run(&db);
        let control = RunControl::new();
        let gate = control.question_gate();
        let (tx, rx) = channel();

        let args = AskQuestionArgs {
            question: "Which target?".to_string(),
            options: vec!["Rust".to_string(), "TypeScript".to_string()],
            multi_select: false,
            allow_free_text: true,
        };

        let gate_bg = gate.clone();
        let db_bg = db.clone();
        let handle = thread::spawn(move || {
            gate_bg.ask_and_wait(&db_bg, run_id, conv_id, "call_q1", &args, Some(&tx))
        });

        let requested = rx.recv().expect("QuestionRequested event");
        let q_id = match requested {
            AgentRunEvent::QuestionRequested { question } => {
                assert_eq!(question.run_id, run_id);
                assert_eq!(question.conversation_id, conv_id);
                assert_eq!(question.options, vec!["Rust", "TypeScript"]);
                question.question_id
            }
            other => panic!("expected QuestionRequested, got {other:?}"),
        };

        let pending_list = list_pending_for_conversation(&db, conv_id).expect("list pending");
        assert_eq!(pending_list.len(), 1);
        assert_eq!(pending_list[0].status, "pending");

        // Validation checks before answering validly
        assert!(matches!(
            gate.resolve(&db, q_id, vec!["Invalid".into()], None),
            Err(QuestionResolveError::InvalidAnswer(_))
        ));
        assert!(matches!(
            gate.resolve(&db, q_id, vec!["Rust".into(), "TypeScript".into()], None),
            Err(QuestionResolveError::InvalidAnswer(_))
        ));
        assert!(matches!(
            gate.resolve(&db, q_id, Vec::new(), None),
            Err(QuestionResolveError::InvalidAnswer(_))
        ));

        let updated = gate
            .resolve(&db, q_id, vec!["Rust".into()], Some("with tests".into()))
            .expect("valid resolve");
        assert_eq!(updated.status, "answered");

        let answer = handle.join().expect("thread join").expect("answer");
        assert_eq!(answer.selected_options, vec!["Rust"]);
        assert_eq!(answer.free_text.as_deref(), Some("with tests"));

        let resolved_ev = rx.recv().expect("QuestionResolved event");
        assert_eq!(
            resolved_ev,
            AgentRunEvent::QuestionResolved {
                question_id: q_id,
                call_id: "call_q1".to_string(),
                status: "answered".to_string(),
            }
        );
        assert!(list_pending_for_conversation(&db, conv_id)
            .expect("list pending after answer")
            .is_empty());
    }

    #[test]
    fn cancelling_control_unblocks_parked_question_cleanly() {
        let db = in_memory_database();
        let (conv_id, run_id) = seed_conv_and_run(&db);
        let control = RunControl::new();
        let gate = control.question_gate();
        let (tx, rx) = channel();

        let args = AskQuestionArgs {
            question: "Proceed?".to_string(),
            options: vec!["Yes".to_string()],
            multi_select: false,
            allow_free_text: false,
        };

        let gate_bg = gate.clone();
        let db_bg = db.clone();
        let handle = thread::spawn(move || {
            gate_bg.ask_and_wait(&db_bg, run_id, conv_id, "call_q2", &args, Some(&tx))
        });

        let _ = rx.recv().expect("QuestionRequested");
        control.cancel();

        let res = handle.join().expect("join");
        assert_eq!(res, Err(QuestionWaitEnd::Cancelled));

        let rows = list_for_conversation(&db, conv_id).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "cancelled");
    }
}
