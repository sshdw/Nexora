//! Opt-in agent run persistence (ROADMAP.md Phase 4 — Task 4.2).
//!
//! This module bridges the [`AgentRunner`] loop to the `agent_runs` /
//! `agent_steps` tables (DATABASE.md §7.8, §7.9) through
//! [`crate::infrastructure::repository::agent_runs::AgentRunRepository`]. It
//! is strictly opt-in: a [`RunRecorder`] must be attached to the runner via
//! `AgentRunner::with_run_recorder`, and when none is attached nothing is
//! written and the loop keeps the exact pre-4.2 behaviour.
//!
//! # Recording semantics (D12)
//!
//! - **Start:** one `agent_runs` row is inserted at the beginning of `run()`
//!   with `status = 'running'` and the recorder's `conversation_id` (linked
//!   runs, D50; Task 5.1 — the IPC layer pre-creates the row with the
//!   conversation and the spawned run adopts it).
//! - **Mode:** the mode recorded for the run is the attached
//!   [`ApprovalGate`]'s current [`AutonomyMode`]. Without a gate the run is
//!   recorded as `supervised` ([`DEFAULT_RECORDED_MODE`]) — the most
//!   conservative rung of the HD-3 ladder — because a run that can never
//!   auto-execute a mutating tool behaves exactly like a supervised one.
//! - **Steps:** append-only `agent_steps` rows with monotonically increasing
//!   `seq` starting at 1, one per model turn (after the provider returns),
//!   per dispatched tool call (raw arguments, observation, and
//!   `succeeded` / `failed` / `cancelled` outcome), and per parked approval
//!   decision (`succeeded` when approved, `denied` when denied, `cancelled`
//!   when cancellation ended the wait).
//! - **Finalize:** on every exit path the run row is finalized — `Ok`
//!   content → `completed` + `final_content`; [`AgentError::Cancelled`] →
//!   `cancelled`; [`AgentError::BudgetExhausted`] → `budget_exhausted`;
//!   [`AgentError::EmptyResponse`] / [`AgentError::Provider`] → `error` +
//!   the classified error `Display` text (never a secret,
//!   DATABASE.md §14).
//!
//! # Best-effort failure policy
//!
//! Persistence failures never panic the loop and never change the run's
//! user-visible semantics: a failed start means the run continues without
//! persistence, a failed step append is logged and skipped (the `seq`
//! counter does not advance, keeping successful inserts gap-free), and a
//! failed finalize is logged. Every failure is reported through `log`.

use std::sync::mpsc::Sender;

use crate::application::agent::approval::AutonomyMode;
use crate::application::agent::control::AgentRunEvent;
use crate::application::agent::runner::AgentError;
use crate::application::execution::ToolCall;
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::agent_runs::AgentRunRepository;

/// Autonomy mode recorded for a run when no [`ApprovalGate`] is attached
/// (Task 4.2): the most conservative rung of the HD-3 ladder, since a run
/// without a gate can never auto-execute a mutating tool through the ladder.
pub(crate) const DEFAULT_RECORDED_MODE: &str = "supervised";

/// Map an [`AutonomyMode`] onto the `agent_runs.mode` column value
/// (DATABASE.md §7.8). Kept beside the recorder so the ladder and the column
/// enumeration cannot drift apart silently.
#[must_use]
pub(crate) fn mode_to_column(mode: AutonomyMode) -> &'static str {
    match mode {
        AutonomyMode::Supervised => "supervised",
        AutonomyMode::SemiAutonomous => "semi_autonomous",
        AutonomyMode::FullAutonomous => "full_autonomous",
    }
}

/// Map a run outcome onto the terminal `agent_runs` columns
/// `(status, final_content, error)` (DATABASE.md §7.8). Shared by
/// [`ActiveRunRecord::finalize`] and the Task 5.1 bridge's `RunFinished`
/// frame, so the UI, the live stream, and the persisted row cannot disagree.
#[must_use]
pub(crate) fn terminal_outcome(
    result: &Result<String, AgentError>,
) -> (&'static str, Option<String>, Option<String>) {
    match result {
        Ok(content) => ("completed", Some(content.clone()), None),
        // Cancellation, budget and spend exhaustion are terminal states of
        // the governance ladder, not classified errors; `error` stays NULL
        // per DATABASE.md §7.8 (terminal `error` only).
        Err(AgentError::Cancelled) => ("cancelled", None, None),
        Err(AgentError::BudgetExhausted(_)) => ("budget_exhausted", None, None),
        Err(AgentError::SpendLimitExceeded { .. }) => ("spend_limit_exceeded", None, None),
        Err(err) => ("error", None, Some(err.to_string())),
    }
}

/// Opt-in run recorder: the attachment point between the runner loop and the
/// agent persistence tables (Task 4.2).
///
/// A cheap copyable handle over the shared application [`Database`], mirroring
/// the runner's other optional attachments (`RunControl`, `ApprovalGate`).
/// All recording is best-effort; see the module docs for the failure policy.
///
/// Task 5.1 extends the recorder with two opt-in additions:
///
/// - `conversation_id`: carried into `create_run` so runs are linked to
///   their conversations (D50; the column existed nullable until now).
/// - `events`: an optional [`Sender<AgentRunEvent>`] borrowed from the run
///   bridge; after each *successful* `append_step` the recorder emits one
///   `AgentRunEvent::StepRecorded` with the persisted `run_id`/`seq`, so the
///   live stream and the persisted rows are produced by the same code path
///   (CF-01-safe: a failed insert emits nothing and its `seq` is reused).
/// - `run_id`: a pre-assigned run id adopted instead of inserting a new row
///   (the Task 5.1 IPC layer creates the row synchronously before spawning
///   the run thread so it can register the run and return the id).
///
/// The runner itself is untouched by all three: it keeps calling
/// `ActiveRunRecord::start(*recorder, ..)` exactly as in 4.2.
#[derive(Clone, Copy)]
pub(crate) struct RunRecorder<'a> {
    db: &'a Database,
    conversation_id: Option<i64>,
    adopted_run_id: Option<i64>,
    events: Option<&'a Sender<AgentRunEvent>>,
}

impl<'a> RunRecorder<'a> {
    /// Create a recorder over the shared application [`Database`].
    #[must_use]
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self {
            db,
            conversation_id: None,
            adopted_run_id: None,
            events: None,
        }
    }

    /// Link recorded runs to `conversation_id` (Task 5.1, D50): the value is
    /// passed straight into `create_run`.
    #[must_use]
    pub(crate) const fn with_conversation(mut self, conversation_id: i64) -> Self {
        self.conversation_id = Some(conversation_id);
        self
    }

    /// Adopt a pre-created `agent_runs` row (Task 5.1): `start` records
    /// against this id instead of inserting a new row.
    #[must_use]
    pub(crate) const fn with_run_id(mut self, run_id: i64) -> Self {
        self.adopted_run_id = Some(run_id);
        self
    }

    /// Stream one `StepRecorded` event per successfully persisted step to
    /// `events` (Task 5.1). Best-effort: a disconnected receiver never blocks
    /// or fails the run.
    #[must_use]
    pub(crate) const fn with_events(mut self, events: &'a Sender<AgentRunEvent>) -> Self {
        self.events = Some(events);
        self
    }

    /// Create the `agent_runs` row for a run about to start (Task 5.1).
    ///
    /// The IPC layer calls this synchronously *before* spawning the run
    /// thread, so it can register the run and return its id immediately; the
    /// spawned thread's recorder then adopts the row via
    /// [`Self::with_run_id`]. Best-effort: returns `None` when the insert
    /// fails.
    #[must_use]
    pub(crate) fn create_run_row(&self, model: &str, mode: &str) -> Option<i64> {
        self.insert_run(model, mode)
    }

    /// Insert the `agent_runs` row for one run (DATABASE.md §7.8).
    ///
    /// Best-effort: returns `None` (instead of panicking or failing the run)
    /// when the insert fails, so the run continues without persistence.
    fn insert_run(self, model: &str, mode: &str) -> Option<i64> {
        let repo = AgentRunRepository::new(self.db);
        match repo.create_run(self.conversation_id, model, mode) {
            Ok(id) => Some(id),
            Err(err) => {
                log::warn!(
                    "agent run persistence: run start failed, continuing \
                     without persistence: {err}"
                );
                None
            }
        }
    }

    /// Append one `agent_steps` row (DATABASE.md §7.9).
    ///
    /// Reports whether persistence succeeded (`Ok(())`) or failed
    /// (`Err`), so the caller can keep its `seq` / step counters consistent.
    /// Failures are logged here — best-effort: they never panic and are
    /// never propagated into the agent run itself (CF-01).
    #[allow(clippy::too_many_arguments)]
    fn insert_step(
        self,
        run_id: i64,
        seq: i64,
        kind: &str,
        tool_name: Option<&str>,
        arguments: Option<&str>,
        observation: Option<&str>,
        status: Option<&str>,
        duration_ms: Option<i64>,
    ) -> Result<(), DatabaseError> {
        let repo = AgentRunRepository::new(self.db);
        let result = repo.append_step(
            run_id,
            seq,
            kind,
            tool_name,
            arguments,
            observation,
            status,
            duration_ms,
        );
        match &result {
            Err(err) => log::warn!(
                "agent run persistence: step append failed (run {run_id}, \
                 seq {seq}), skipping step: {err}"
            ),
            // Task 5.1: emit one step event per successfully persisted step,
            // with exactly the persisted `run_id`/`seq` (CF-01-aligned). The
            // emission happens on the run thread (recorder methods are called
            // by the runner), so governance and step events on the same
            // channel stay totally ordered by emission. Best-effort: a
            // disconnected receiver never blocks or fails the run.
            Ok(_) => {
                if let Some(events) = self.events {
                    let _ = events.send(AgentRunEvent::StepRecorded {
                        run_id,
                        seq,
                        kind: kind.to_string(),
                        tool_name: tool_name.map(str::to_string),
                        arguments: arguments.map(str::to_string),
                        observation: observation.map(str::to_string),
                        status: status.map(str::to_string),
                        duration_ms,
                    });
                }
            }
        }
        result.map(|_| ())
    }

    /// Finalize the `agent_runs` row at run termination (DATABASE.md §7.8).
    /// Best-effort.
    #[allow(clippy::too_many_arguments)]
    fn finalize_run(
        self,
        run_id: i64,
        status: &str,
        total_steps: i64,
        final_content: Option<&str>,
        error: Option<&str>,
        spent_micro_usd: Option<u64>,
        limit_micro_usd: Option<u64>,
    ) {
        let repo = AgentRunRepository::new(self.db);
        if let Err(err) = repo.finalize_run(
            run_id,
            status,
            total_steps,
            final_content,
            error,
            spent_micro_usd,
            limit_micro_usd,
        ) {
            log::warn!(
                "agent run persistence: finalize failed (run {run_id}), \
                 continuing: {err}"
            );
        }
    }
}

/// Per-run recording state owned by one `AgentRunner::run` invocation
/// (Task 4.2): the run row id, the next step `seq`, and the number of
/// successfully recorded steps (`total_steps`, D12).
pub(crate) struct ActiveRunRecord<'a> {
    recorder: RunRecorder<'a>,
    run_id: Option<i64>,
    next_seq: i64,
    recorded_steps: i64,
}

impl<'a> ActiveRunRecord<'a> {
    /// Start recording one run: record against the adopted run id when the
    /// recorder carries one (Task 5.1 IPC path — the row was created before
    /// the run thread spawned), otherwise insert the `agent_runs` row
    /// (DATABASE.md §7.8). Best-effort — a failed start leaves the record
    /// without a run id and every later step append becomes a no-op.
    #[must_use]
    pub(crate) fn start(recorder: RunRecorder<'a>, model: &str, mode: &str) -> Self {
        let run_id = recorder
            .adopted_run_id
            .or_else(|| recorder.insert_run(model, mode));
        Self {
            recorder,
            run_id,
            next_seq: 1,
            recorded_steps: 0,
        }
    }

    /// Record one `model_turn` step: the provider returned. `observation`
    /// carries the model's own narration / final text; `duration_ms` the
    /// provider round-trip duration (DATABASE.md §7.9).
    pub(crate) fn model_turn(&mut self, observation: &str, duration_ms: Option<i64>) {
        self.append(
            "model_turn",
            None,
            None,
            Some(observation),
            None,
            duration_ms,
        );
    }

    /// Record one `tool_call` step for a dispatched call (DATABASE.md
    /// §7.9). `arguments` is the raw JSON exactly as provider-supplied,
    /// `status` one of `succeeded` / `failed` / `cancelled`.
    pub(crate) fn tool_call(
        &mut self,
        call: &ToolCall,
        observation: &str,
        status: &str,
        duration_ms: Option<i64>,
    ) {
        self.append(
            "tool_call",
            Some(&call.name),
            Some(&call.arguments),
            Some(observation),
            Some(status),
            duration_ms,
        );
    }

    /// Record one `approval` step for a parked approval decision resolved by
    /// the user (DATABASE.md §7.9): `succeeded` when approved, `denied` when
    /// denied.
    pub(crate) fn approval(&mut self, call: &ToolCall, approved: bool) {
        let (status, observation) = if approved {
            ("succeeded", "approved")
        } else {
            ("denied", "denied")
        };
        self.append(
            "approval",
            Some(&call.name),
            Some(&call.arguments),
            Some(observation),
            Some(status),
            None,
        );
    }

    /// Record one `approval` step whose parked wait was ended by
    /// cancellation (DATABASE.md §7.9).
    pub(crate) fn approval_cancelled(&mut self, call: &ToolCall) {
        self.append(
            "approval",
            Some(&call.name),
            Some(&call.arguments),
            Some("cancelled by the user"),
            Some("cancelled"),
            None,
        );
    }

    /// Append one step with the next `seq`. Best-effort: a failed insert is
    /// logged and skipped without advancing the counters, so the next step
    /// reuses the same `seq`, `total_steps` counts only successfully
    /// persisted steps, and successful inserts stay gap-free and strictly
    /// increasing (D12; CF-01).
    fn append(
        &mut self,
        kind: &str,
        tool_name: Option<&str>,
        arguments: Option<&str>,
        observation: Option<&str>,
        status: Option<&str>,
        duration_ms: Option<i64>,
    ) {
        let Some(run_id) = self.run_id else {
            return;
        };
        let seq = self.next_seq;
        if self
            .recorder
            .insert_step(
                run_id,
                seq,
                kind,
                tool_name,
                arguments,
                observation,
                status,
                duration_ms,
            )
            .is_ok()
        {
            self.next_seq += 1;
            self.recorded_steps += 1;
        }
    }

    /// Finalize the run on every exit path (DATABASE.md §7.8): `completed`
    /// with final content, `cancelled`, `budget_exhausted`,
    /// `spend_limit_exceeded`, or `error` with the classified error text.
    /// `total_steps` is the number of recorded steps (D12). Best-effort.
    pub(crate) fn finalize(
        &self,
        result: &Result<String, AgentError>,
        spent_micro_usd: u64,
        limit_micro_usd: Option<u64>,
    ) {
        let Some(run_id) = self.run_id else {
            return;
        };
        let (status, final_content, error) = terminal_outcome(result);
        // Task 4.3: spent/limit are persisted when a recorder is attached,
        // even for non-spend runs (spent may be 0). Pre-v5 rows have NULL.
        let spent_opt = Some(spent_micro_usd);
        self.recorder.finalize_run(
            run_id,
            status,
            self.recorded_steps,
            final_content.as_deref(),
            error.as_deref(),
            spent_opt,
            limit_micro_usd,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;
    use rusqlite::params;

    /// CF-01: a failed step append must not advance `next_seq` /
    /// `recorded_steps`; the next successful step reuses the same `seq`,
    /// successfully persisted steps stay gap-free, and `total_steps` counts
    /// only successfully persisted steps.
    ///
    /// The failure is simulated deterministically through the smallest
    /// existing seam: the shared connection behind [`Database::lock`] (the
    /// same seam the repository and schema tests use) pre-occupies seq 1 so
    /// the recorder's first insert fails on the schema's
    /// `UNIQUE(run_id, seq)` constraint.
    #[test]
    fn failed_step_append_preserves_seq_and_counts_only_persisted_steps() {
        let db = in_memory_database();
        let mut record = ActiveRunRecord::start(RunRecorder::new(&db), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        // Force the FIRST step append to fail.
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind) VALUES (?1, 1, 'model_turn')",
                params![run_id],
            )
            .expect("occupy seq 1");
        }

        record.model_turn("turn one", None);

        // The failure was swallowed (best-effort: no panic, no propagation
        // into the run), but neither counter advanced.
        assert_eq!(record.next_seq, 1, "seq does not advance on failure");
        assert_eq!(
            record.recorded_steps, 0,
            "recorded_steps does not advance on failure"
        );

        // Clear the obstacle so the next append can succeed; it must reuse
        // the same sequence number 1.
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "DELETE FROM agent_steps WHERE run_id = ?1 AND seq = 1",
                params![run_id],
            )
            .expect("clear seq 1");
        }

        record.model_turn("turn one (persisted)", None);
        record.model_turn("turn two", None);

        assert_eq!(record.next_seq, 3, "successful steps advance the seq");
        assert_eq!(record.recorded_steps, 2);

        let runs = AgentRunRepository::new(&db);
        let steps = runs.list_steps(run_id).expect("list steps");
        let seqs: Vec<i64> = steps.iter().map(|s| s.seq).collect();
        assert_eq!(
            seqs,
            vec![1, 2],
            "successfully persisted steps stay gap-free"
        );

        record.finalize(&Ok("done".to_string()), 0, None);
        let run = runs.read_run(run_id).expect("read run").expect("exists");
        assert_eq!(
            run.total_steps, 2,
            "total_steps counts only successfully persisted steps"
        );
    }

    /// Task 5.1: the recorder emits one `StepRecorded` per successfully
    /// persisted step, with `seq` exactly matching the persisted rows and
    /// payloads identical to the stored columns.
    #[test]
    fn step_events_align_with_persisted_seq_and_payloads() {
        let db = in_memory_database();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut record =
            ActiveRunRecord::start(RunRecorder::new(&db).with_events(&tx), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        record.model_turn("thinking", Some(120));
        let call = ToolCall {
            id: "c1".to_string(),
            name: "read_file".to_string(),
            arguments: "{}".to_string(),
            thought_signature: None,
        };
        record.tool_call(&call, "file body", "succeeded", Some(5));
        record.approval(&call, true);

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert_eq!(events.len(), 3, "one event per persisted step");
        for (index, event) in events.iter().enumerate() {
            let AgentRunEvent::StepRecorded {
                run_id: event_run_id,
                seq,
                ..
            } = event
            else {
                panic!("expected StepRecorded, got: {event:?}");
            };
            assert_eq!(*event_run_id, run_id);
            assert_eq!(
                *seq,
                i64::try_from(index + 1).expect("index fits in i64"),
                "seq aligns with the row"
            );
        }
        let AgentRunEvent::StepRecorded {
            kind,
            tool_name,
            observation,
            status,
            duration_ms,
            ..
        } = &events[2]
        else {
            unreachable!("checked above");
        };
        assert_eq!(kind, "approval");
        assert_eq!(tool_name.as_deref(), Some("read_file"));
        assert_eq!(observation.as_deref(), Some("approved"));
        assert_eq!(status.as_deref(), Some("succeeded"));
        assert_eq!(*duration_ms, None);

        // The live seqs are exactly the persisted seqs.
        let runs = AgentRunRepository::new(&db);
        let persisted: Vec<i64> = runs
            .list_steps(run_id)
            .expect("list steps")
            .iter()
            .map(|step| step.seq)
            .collect();
        let streamed: Vec<i64> = events
            .iter()
            .map(|event| match event {
                AgentRunEvent::StepRecorded { seq, .. } => *seq,
                other => panic!("unexpected event: {other:?}"),
            })
            .collect();
        assert_eq!(streamed, persisted, "live stream matches persisted rows");
    }

    /// Task 5.1 + CF-01: a failed step insert emits NO event; the retry that
    /// succeeds reuses the same `seq` and emits it once.
    #[test]
    fn failed_step_insert_emits_no_event_and_retry_uses_same_seq() {
        let db = in_memory_database();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut record =
            ActiveRunRecord::start(RunRecorder::new(&db).with_events(&tx), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        // Force the FIRST step append to fail (same seam as the CF-01 test).
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "INSERT INTO agent_steps (run_id, seq, kind) VALUES (?1, 1, 'model_turn')",
                params![run_id],
            )
            .expect("occupy seq 1");
        }
        record.model_turn("dropped", None);
        assert!(rx.try_recv().is_err(), "a failed insert must emit no event");

        // Clear the obstacle; the retry reuses seq 1 and emits it once.
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "DELETE FROM agent_steps WHERE run_id = ?1 AND seq = 1",
                params![run_id],
            )
            .expect("clear seq 1");
        }
        record.model_turn("persisted", None);
        match rx.try_recv().expect("retry emits") {
            AgentRunEvent::StepRecorded { seq, .. } => assert_eq!(seq, 1),
            other => panic!("expected StepRecorded, got: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "no duplicate event");
    }

    /// Task 5.1 (D50): the recorder passes its conversation id into
    /// `create_run`; a recorder without one keeps the historical NULL.
    #[test]
    fn conversation_id_is_passed_to_create_run() {
        let db = in_memory_database();
        let conversations =
            crate::infrastructure::repository::conversations::ConversationRepository::new(&db);
        let conversation_id = conversations
            .create("conv", "active")
            .expect("conversation");

        let linked = ActiveRunRecord::start(
            RunRecorder::new(&db).with_conversation(conversation_id),
            "m",
            "supervised",
        );
        let linked_id = linked.run_id.expect("linked run persisted");
        let runs = AgentRunRepository::new(&db);
        assert_eq!(
            runs.read_run(linked_id)
                .expect("read")
                .expect("exists")
                .conversation_id,
            Some(conversation_id),
            "create_run receives the recorder's conversation id"
        );

        let unlinked = ActiveRunRecord::start(RunRecorder::new(&db), "m", "supervised");
        let unlinked_id = unlinked.run_id.expect("run persisted");
        assert_eq!(
            runs.read_run(unlinked_id)
                .expect("read")
                .expect("exists")
                .conversation_id,
            None,
            "no conversation id recorded without with_conversation"
        );
    }

    /// Task 5.1: an adopted run id records against the pre-created row —
    /// no second `agent_runs` row is inserted and steps land under the
    /// adopted id.
    #[test]
    fn adopted_run_id_records_against_the_pre_created_row() {
        let db = in_memory_database();
        let runs = AgentRunRepository::new(&db);
        let conversations =
            crate::infrastructure::repository::conversations::ConversationRepository::new(&db);
        let conversation_id = conversations
            .create("conv", "active")
            .expect("conversation");
        let pre_created = runs
            .create_run(Some(conversation_id), "m", "semi_autonomous")
            .expect("pre-created row");

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut record = ActiveRunRecord::start(
            RunRecorder::new(&db)
                .with_run_id(pre_created)
                .with_conversation(conversation_id)
                .with_events(&tx),
            "m",
            "semi_autonomous",
        );
        record.model_turn("turn", None);

        assert_eq!(record.run_id, Some(pre_created), "adopted id is kept");
        assert_eq!(
            runs.list_runs_by_conversation(conversation_id)
                .expect("list")
                .len(),
            1,
            "no duplicate run row is inserted"
        );
        let steps = runs.list_steps(pre_created).expect("steps");
        assert_eq!(steps.len(), 1, "steps land under the adopted run id");
        assert_eq!(steps[0].seq, 1);
    }
}
