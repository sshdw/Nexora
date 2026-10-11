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
//!   per dispatched tool call (redacted arguments, bounded observation, and
//!   `succeeded` / `failed` / `cancelled` outcome), and per parked approval
//!   decision (`succeeded` when approved, `denied` when denied, `cancelled`
//!   when cancellation ended the wait). Arguments and observations are
//!   secret-scrubbed and length-bounded before insert (NEX-SEC-003), so raw
//!   tool I/O — env output, file reads, possible key material — never lands
//!   in plaintext `SQLite`.
//! - **Finalize:** on every exit path the run row is finalized — `Ok`
//!   content → `completed` + `final_content`; [`AgentError::Cancelled`] →
//!   `cancelled`; [`AgentError::BudgetExhausted`] → `budget_exhausted`;
//!   [`AgentError::EmptyResponse`] / [`AgentError::Provider`] /
//!   [`AgentError::ContextExhausted`] → `error` + the classified error
//!   `Display` text (never a secret, DATABASE.md §14). `ContextExhausted`
//!   keeps the `error` status but carries a distinct Display text so the UI
//!   can tell exhausted recovery apart from a retryable overflow.
//!
//! # Best-effort failure policy
//!
//! Persistence failures never panic the loop and never change the run's
//! user-visible semantics: a failed start means the run continues without
//! persistence, a failed step append is logged and skipped (the `seq`
//! counter does not advance, keeping successful inserts gap-free), and a
//! failed finalize is logged. Every failure is reported through `log`.

use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::application::agent::approval::format_timeout;
use crate::application::agent::approval::AutonomyMode;
use crate::application::agent::control::AgentRunEvent;
use crate::application::agent::injection::redact_secrets;
use crate::application::agent::runner::AgentError;
use crate::application::execution::{TokenUsage, ToolCall};
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::agent_runs::AgentRunRepository;

/// Maximum persisted step-text size in chars (NEX-SEC-003): tool arguments
/// and observations are unbounded (a `write_file` call carries the whole
/// file, `execute_command` the whole stdout), while `agent_steps` is a
/// plaintext transcript, not an artifact store. The cap sits above the tool
/// layer's shaped-output ceiling (every observation passes through
/// `truncate_output`: 20KB kept plus a short spill notice), so shaped
/// observations — spill notices included — persist byte-identical and only
/// genuinely oversized text is cut. Text past the cap is cut with an honest
/// `[truncated N of M chars]` marker so readers can tell a bound hit from
/// complete output.
pub(crate) const MAX_PERSISTED_STEP_TEXT_CHARS: usize = 32_768;

/// Scrub probable secrets ([`redact_secrets`]) then enforce
/// [`MAX_PERSISTED_STEP_TEXT_CHARS`] on one `arguments` / `observation`
/// payload before it reaches `SQLite` or the `StepRecorded` stream
/// (NEX-SEC-003). Pure and total: short benign text round-trips
/// byte-identical, so transcript fidelity only changes where a secret
/// pattern or the length cap fires.
fn sanitize_persisted_text(text: &str) -> String {
    let (scrubbed, _) = redact_secrets(text);
    bound_persisted_text(&scrubbed)
}

/// Cut `text` to [`MAX_PERSISTED_STEP_TEXT_CHARS`] chars with an honest
/// marker naming the omitted and total counts, so a bound hit is
/// distinguishable from complete output. Char-based (never splits UTF-8);
/// text at or under the cap returns unchanged.
fn bound_persisted_text(text: &str) -> String {
    let total = text.chars().count();
    if total <= MAX_PERSISTED_STEP_TEXT_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_PERSISTED_STEP_TEXT_CHARS).collect();
    let omitted = total - MAX_PERSISTED_STEP_TEXT_CHARS;
    format!("{kept}\n[truncated {omitted} of {total} chars]")
}

/// Maximum live-streamed step-text size in chars (NEX-PERF-003): a
/// `StepRecorded` frame carries only a *preview* of the sanitized stored
/// text, so a long run stays bounded over IPC and in the render tree no
/// matter how many turns it takes. The cap sits far below the persist cap
/// (32K chars) and matches the UI's long-text threshold, so short steps —
/// the common case — stream byte-identical to what is stored and only
/// genuinely large outputs travel as preview + on-demand full text. The
/// preview is always derived from the *sanitized* text (post
/// [`sanitize_persisted_text`]), never pre-redaction.
pub(crate) const MAX_STEP_PREVIEW_CHARS: usize = 2_000;

/// Derive the live-streamed `(preview, total_bytes, truncated)` triple from
/// one *sanitized* observation (NEX-PERF-003). `total_bytes` is the
/// byte-exact length of the full sanitized text; `truncated` reports whether
/// the preview was cut. Text at or under [`MAX_STEP_PREVIEW_CHARS`] chars
/// returns unchanged with `truncated = false`, so short steps are
/// byte-identical across the stream, the row, and the on-demand fetch.
/// Longer text keeps the first [`MAX_STEP_PREVIEW_CHARS`] chars (char-based,
/// never splits UTF-8 — the kept prefix is collected from `chars`, so it
/// always ends on a char boundary) plus an honest
/// `[truncated N of M bytes]` marker where both counts are bytes, matching
/// the byte unit of `total_bytes` (and of the UI's full-output button), so
/// the two numbers on one step can never disagree on multibyte output.
pub(crate) fn step_preview(sanitized: &str) -> (String, u64, bool) {
    let total_bytes = sanitized.len() as u64;
    if sanitized.chars().count() <= MAX_STEP_PREVIEW_CHARS {
        return (sanitized.to_string(), total_bytes, false);
    }
    let kept: String = sanitized.chars().take(MAX_STEP_PREVIEW_CHARS).collect();
    let kept_bytes = kept.len() as u64;
    let omitted = total_bytes - kept_bytes;
    (
        format!("{kept}\n[truncated {omitted} of {total_bytes} bytes]"),
        total_bytes,
        true,
    )
}

/// Bounded-preview projection of an optional sanitized observation
/// (NEX-PERF-003): `None` maps to `(None, None, false)`, `Some` text maps
/// through [`step_preview`]. Shared by the live `StepRecorded` emission in
/// [`ActiveRunRecord::insert_step`] and the read-time
/// [`super::service::AgentStepListItem::from_row`] projection, so the two
/// paths cannot drift apart. Pure: no behavior change beyond the shared
/// derivation.
pub(crate) fn step_preview_opt(sanitized: Option<&str>) -> (Option<String>, Option<u64>, bool) {
    sanitized.map_or((None, None, false), |text| {
        let (preview, total_bytes, truncated) = step_preview(text);
        (Some(preview), Some(total_bytes), truncated)
    })
}

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
    // WS-B.1 lifecycle consult: every terminal maps to a legal
    // `Running -> terminal` transition. Observe-only; outcome unchanged.
    let terminal = match result {
        Ok(_) => super::lifecycle::RunState::Completed,
        Err(AgentError::Cancelled) => super::lifecycle::RunState::Cancelled,
        Err(AgentError::BudgetExhausted(_)) => super::lifecycle::RunState::BudgetExhausted,
        Err(AgentError::SpendLimitExceeded { .. }) => {
            super::lifecycle::RunState::SpendLimitExceeded
        }
        Err(_) => super::lifecycle::RunState::Failed,
    };
    super::lifecycle::observe_transition(super::lifecycle::RunState::Running, terminal);
    match result {
        Ok(content) => ("completed", Some(content.clone()), None),
        // Cancellation, budget and spend exhaustion are terminal states of
        // the governance ladder, not classified errors; `error` stays NULL
        // per DATABASE.md §7.8 (terminal `error` only).
        Err(AgentError::Cancelled) => ("cancelled", None, None),
        Err(AgentError::BudgetExhausted(_)) => ("budget_exhausted", None, None),
        Err(AgentError::SpendLimitExceeded { .. }) => ("spend_limit_exceeded", None, None),
        // `ContextExhausted` falls through to the `error` arm below: it
        // keeps the `error` status (no schema change) and its distinct
        // Display text lets the UI tell exhausted recovery apart from a
        // retryable overflow.
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
    /// NEX-SEC-003: `arguments` and `observation` are secret-scrubbed
    /// ([`redact_secrets`]) and length-bounded before insert, so the row —
    /// and the `StepRecorded` event below, which carries the same sanitized
    /// payloads — can never smuggle key material into `SQLite`, IPC, or the
    /// logs. (No payload is ever logged on this path: failures report only
    /// `run_id`/`seq` plus the secret-free [`DatabaseError`].)
    ///
    /// NEX-PERF-003: the emitted event carries a *bounded preview* of the
    /// sanitized observation ([`step_preview`]) plus its byte-exact total —
    /// never the full output — while the row keeps the full sanitized text
    /// for the on-demand fetch.
    ///
    /// Reports the new row's `agent_steps.id` (`Ok`) or the failure (`Err`),
    /// so the caller can keep its `seq` / step counters consistent and the
    /// event can name the row for the on-demand fetch. Failures are logged
    /// here — best-effort: they never panic and are never propagated into
    /// the agent run itself (CF-01).
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
        rule_id: Option<i64>,
        group_key: Option<&str>,
        decided_by: Option<&str>,
        usage: Option<TokenUsage>,
    ) -> Result<i64, DatabaseError> {
        let repo = AgentRunRepository::new(self.db);
        let arguments = arguments.map(sanitize_persisted_text);
        let observation = observation.map(sanitize_persisted_text);
        let result = repo.append_step(
            run_id,
            seq,
            kind,
            tool_name,
            arguments.as_deref(),
            observation.as_deref(),
            status,
            duration_ms,
            rule_id,
            group_key,
            decided_by,
            usage.map(|u| u.input_tokens),
            usage.map(|u| u.output_tokens),
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
            //
            // NEX-PERF-003: the event carries the row id (for the on-demand
            // full-text fetch) and a bounded preview of the sanitized
            // observation plus its byte-exact total — derived from the same
            // sanitized text that was stored, never pre-redaction.
            Ok(step_id) => {
                if let Some(events) = self.events {
                    let (preview, total_bytes, truncated) =
                        step_preview_opt(observation.as_deref());
                    let _ = events.send(AgentRunEvent::StepRecorded {
                        step_id: *step_id,
                        run_id,
                        seq,
                        kind: kind.to_string(),
                        tool_name: tool_name.map(str::to_string),
                        arguments,
                        observation: preview,
                        observation_len: total_bytes,
                        observation_truncated: truncated,
                        status: status.map(str::to_string),
                        duration_ms,
                    });
                }
            }
        }
        result
    }

    /// Finalize the `agent_runs` row at run termination (DATABASE.md §7.8).
    /// Best-effort.
    #[allow(clippy::too_many_arguments)]
    fn finalize_run(
        self,
        run_id: i64,
        status: &str,
        final_content: Option<&str>,
        error: Option<&str>,
        spent_micro_usd: Option<u64>,
        limit_micro_usd: Option<u64>,
    ) {
        let repo = AgentRunRepository::new(self.db);
        if let Err(err) = repo.finalize_run(
            run_id,
            status,
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

/// M1-core provenance triple carried on `agent_steps` rows (`rule_id` /
/// `group_key` / `decided_by`, all `None` for pre-v7 rows and ladder-auto
/// model turns).
#[derive(Debug, Clone, Copy)]
pub(crate) struct StepProvenance<'a> {
    /// Authoring permission rule, if the decision came from one.
    pub rule_id: Option<i64>,
    /// Session-sticky group key (`preset:tool:parent-or-*`), if grouped.
    pub group_key: Option<&'a str>,
    /// Who decided: `'user'` / `'rule'` / `'system'`.
    pub decided_by: Option<&'a str>,
}

impl<'a> StepProvenance<'a> {
    /// No provenance (pre-v7 shape; ladder-auto model turns).
    #[must_use]
    pub(crate) const fn anonymous() -> Self {
        Self {
            rule_id: None,
            group_key: None,
            decided_by: None,
        }
    }

    /// A parked user resolution, optionally grouped.
    #[must_use]
    pub(crate) const fn user(group_key: Option<&'a str>) -> Self {
        Self {
            rule_id: None,
            group_key,
            decided_by: Some("user"),
        }
    }

    /// A parked user resolution where a permission rule matched but did not
    /// authorize execution on its own (NEX-AGENT-012 honest surfacing): the
    /// user decides (`decided_by='user'`), while `rule_id` names the
    /// considered rule so the ledger shows which rule caused the park.
    #[must_use]
    pub(crate) const fn user_with_rule(group_key: Option<&'a str>, rule_id: Option<i64>) -> Self {
        Self {
            rule_id,
            group_key,
            decided_by: Some("user"),
        }
    }

    /// A permission-rule verdict.
    #[must_use]
    pub(crate) const fn rule(rule_id: Option<i64>) -> Self {
        Self {
            rule_id,
            group_key: None,
            decided_by: Some("rule"),
        }
    }

    /// A ladder-auto execution.
    #[must_use]
    pub(crate) const fn system() -> Self {
        Self {
            rule_id: None,
            group_key: None,
            decided_by: Some("system"),
        }
    }
}

/// One not-yet-sequenced `agent_steps` row (M1-core): the step payload plus
/// its provenance triple, sequenced by [`ActiveRunRecord::append`].
struct PendingStep<'a> {
    kind: &'a str,
    tool_name: Option<&'a str>,
    arguments: Option<&'a str>,
    observation: Option<&'a str>,
    status: Option<&'a str>,
    duration_ms: Option<i64>,
    provenance: StepProvenance<'a>,
    /// Provider-reported turn usage (`response.usage`); `None` for
    /// non-model steps and for turns whose provider omitted it.
    usage: Option<TokenUsage>,
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
    /// provider round-trip duration; `usage` the provider-reported turn
    /// usage (`None` when the provider omitted it — persisted as NULL, an
    /// honest unknown, never an estimate) (DATABASE.md §7.9).
    pub(crate) fn model_turn(
        &mut self,
        observation: &str,
        duration_ms: Option<i64>,
        usage: Option<TokenUsage>,
    ) {
        self.append(&PendingStep {
            kind: "model_turn",
            tool_name: None,
            arguments: None,
            observation: Some(observation),
            status: None,
            duration_ms,
            provenance: StepProvenance::anonymous(),
            usage,
        });
    }

    /// Record one `tool_call` step for a dispatched call (DATABASE.md
    /// §7.9). `arguments` is the provider-supplied JSON and `observation`
    /// the tool output; both are secret-scrubbed and length-bounded by
    /// [`sanitize_persisted_text`] before insert (NEX-SEC-003), so what the
    /// loop saw and what `SQLite` keeps differ exactly where redaction or the
    /// cap fired. `status` one of `succeeded` / `failed` / `cancelled`.
    pub(crate) fn tool_call(
        &mut self,
        call: &ToolCall,
        observation: &str,
        status: &str,
        duration_ms: Option<i64>,
    ) {
        self.tool_call_with_provenance(
            call,
            observation,
            status,
            duration_ms,
            StepProvenance::anonymous(),
        );
    }

    /// Record a dispatched `tool_call` with M1-core provenance.
    pub(crate) fn tool_call_with_provenance(
        &mut self,
        call: &ToolCall,
        observation: &str,
        status: &str,
        duration_ms: Option<i64>,
        provenance: StepProvenance<'_>,
    ) {
        self.append(&PendingStep {
            kind: "tool_call",
            tool_name: Some(&call.name),
            arguments: Some(&call.arguments),
            observation: Some(observation),
            status: Some(status),
            duration_ms,
            provenance,
            usage: None,
        });
    }

    /// Record one `approval` step for a parked approval decision resolved by
    /// the user (DATABASE.md §7.9): `succeeded` when approved, `denied` when
    /// denied.
    pub(crate) fn approval(&mut self, call: &ToolCall, approved: bool) {
        self.approval_with_provenance(call, approved, StepProvenance::user(None));
    }

    /// Record an `approval` step with M1-core provenance (`decided_by` is
    /// `'user'` for parked resolutions, `'rule'` for rule denials, `'system'`
    /// for auto/cancel paths; `observation` keeps the frozen user vocabulary
    /// (`approved`/`denied`) and uses `denied by rule:<id>` for the new rule
    /// denial origin).
    pub(crate) fn approval_with_provenance(
        &mut self,
        call: &ToolCall,
        approved: bool,
        provenance: StepProvenance<'_>,
    ) {
        // Frozen user vocabulary (`approved`/`denied`) is preserved for
        // parked resolutions so existing denial asserts stay green; the
        // group/session sticky path records its own `denied by group`
        // observation via `approval_denied_by_group`.
        let (status, observation) = if approved {
            ("succeeded", "approved".to_string())
        } else if provenance.decided_by == Some("rule") {
            if let Some(id) = provenance.rule_id {
                ("denied", format!("denied by rule:{id}"))
            } else {
                ("denied", "denied".to_string())
            }
        } else {
            ("denied", "denied".to_string())
        };
        self.append(&PendingStep {
            kind: "approval",
            tool_name: Some(&call.name),
            arguments: Some(&call.arguments),
            observation: Some(&observation),
            status: Some(status),
            duration_ms: None,
            provenance,
            usage: None,
        });
    }

    /// Record a group-inherited denial (M1-core session-sticky): the
    /// observation uses the group vocabulary while `decided_by` stays
    /// `'user'` (the sticky verdict inherits the original user decision).
    pub(crate) fn approval_denied_by_group(&mut self, call: &ToolCall, group_key: &str) {
        self.append(&PendingStep {
            kind: "approval",
            tool_name: Some(&call.name),
            arguments: Some(&call.arguments),
            observation: Some("denied by group"),
            status: Some("denied"),
            duration_ms: None,
            provenance: StepProvenance::user(Some(group_key)),
            usage: None,
        });
    }

    /// Record one `approval` step whose parked wait was ended by
    /// cancellation (DATABASE.md §7.9).
    pub(crate) fn approval_cancelled(&mut self, call: &ToolCall) {
        self.append(&PendingStep {
            kind: "approval",
            tool_name: Some(&call.name),
            arguments: Some(&call.arguments),
            observation: Some("cancelled by the user"),
            status: Some("cancelled"),
            duration_ms: None,
            provenance: StepProvenance::system(),
            usage: None,
        });
    }

    /// Record one `approval` step whose parked wait was ended by the
    /// NEX-TASK-002 application shutdown (DATABASE.md §7.9). The step keeps
    /// the schema `cancelled` status (the run terminal is `cancelled`), but
    /// the observation names the shutdown honestly: a quit is neither a
    /// user denial nor a user cancel, so it must never reuse the
    /// `"cancelled by the user"` vocabulary (same honesty rule as the
    /// NEX-AGENT-004 timeout). `decided_by` stays `'system'`.
    pub(crate) fn approval_shutdown_cancelled(&mut self, call: &ToolCall) {
        self.append(&PendingStep {
            kind: "approval",
            tool_name: Some(&call.name),
            arguments: Some(&call.arguments),
            observation: Some("approval cancelled by application shutdown; run cancelled"),
            status: Some("cancelled"),
            duration_ms: None,
            provenance: StepProvenance::system(),
            usage: None,
        });
    }

    /// Record one `approval` step whose parked wait was ended by the
    /// NEX-AGENT-004 park deadline (DATABASE.md §7.9). The step keeps the
    /// schema `cancelled` status (the run terminal is `cancelled`), but the
    /// observation names the timeout honestly: an expired deadline is
    /// neither a user denial nor a user cancel, so it must never reuse the
    /// `"cancelled by the user"` vocabulary. `decided_by` stays `'system'`.
    pub(crate) fn approval_timed_out(&mut self, call: &ToolCall, timeout: Duration) {
        self.append(&PendingStep {
            kind: "approval",
            tool_name: Some(&call.name),
            arguments: Some(&call.arguments),
            observation: Some(&format!(
                "approval timed out after {} without a decision; run cancelled",
                format_timeout(timeout)
            )),
            status: Some("cancelled"),
            duration_ms: None,
            provenance: StepProvenance::system(),
            usage: None,
        });
    }

    /// Append one step with the next `seq`. Best-effort: a failed insert is
    /// logged and skipped without advancing the counters, so the next step
    /// reuses the same `seq`, `total_steps` counts only successfully
    /// persisted steps, and successful inserts stay gap-free and strictly
    /// increasing (D12; CF-01).
    fn append(&mut self, step: &PendingStep<'_>) {
        let Some(run_id) = self.run_id else {
            return;
        };
        let seq = self.next_seq;
        if self
            .recorder
            .insert_step(
                run_id,
                seq,
                step.kind,
                step.tool_name,
                step.arguments,
                step.observation,
                step.status,
                step.duration_ms,
                step.provenance.rule_id,
                step.provenance.group_key,
                step.provenance.decided_by,
                step.usage,
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

        record.model_turn("turn one", None, None);

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

        record.model_turn("turn one (persisted)", None, None);
        record.model_turn("turn two", None, None);

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

        record.model_turn("thinking", Some(120), None);
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
        record.model_turn("dropped", None, None);
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
        record.model_turn("persisted", None, None);
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
        record.model_turn("turn", None, None);

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

    /// NEX-AGENT-005: a `model_turn` recorded with provider-reported usage
    /// persists it on the step row; a turn without usage persists NULLs
    /// (honest unknown), so the meter can tell reported from absent.
    #[test]
    fn model_turn_persists_reported_usage_and_nulls_absent_usage() {
        let db = in_memory_database();
        let mut record = ActiveRunRecord::start(RunRecorder::new(&db), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        record.model_turn(
            "reported",
            None,
            Some(TokenUsage {
                input_tokens: 120,
                output_tokens: 34,
            }),
        );
        record.model_turn("unreported", None, None);

        let steps = AgentRunRepository::new(&db)
            .list_steps(run_id)
            .expect("list steps");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].input_tokens, Some(120));
        assert_eq!(steps[0].output_tokens, Some(34));
        assert_eq!(steps[1].input_tokens, None);
        assert_eq!(steps[1].output_tokens, None);
    }

    /// `terminal_outcome` collision pin: `ContextExhausted` (terminal
    /// recovery exhaustion) vs `Provider(ContextLengthExceeded)` (retryable
    /// overflow) vs `BudgetExhausted` (step-budget governance) produce
    /// distinct `(status, error-text)` pairs, so the UI can tell the three
    /// apart without a schema change.
    #[test]
    fn terminal_outcome_keeps_exhaustion_variants_distinct() {
        use crate::application::execution::ExecutorError;

        let (exhausted_status, exhausted_content, exhausted_error) =
            terminal_outcome(&Err(AgentError::ContextExhausted));
        let (overflow_status, overflow_content, overflow_error) = terminal_outcome(&Err(
            AgentError::Provider(ExecutorError::ContextLengthExceeded),
        ));
        let (budget_status, budget_content, budget_error) =
            terminal_outcome(&Err(AgentError::BudgetExhausted(10)));

        assert_eq!(exhausted_status, "error");
        assert_eq!(overflow_status, "error");
        assert_eq!(budget_status, "budget_exhausted");
        assert_eq!(exhausted_content, None);
        assert_eq!(overflow_content, None);
        assert_eq!(budget_content, None);
        assert_eq!(budget_error, None);

        let exhausted_text = exhausted_error.expect("exhausted carries error text");
        let overflow_text = overflow_error.expect("overflow carries error text");
        assert_ne!(
            exhausted_text, overflow_text,
            "exhausted recovery and retryable overflow must differ"
        );
        assert!(
            exhausted_text.contains("exhausted"),
            "exhausted Display names the exhaustion, got {exhausted_text:?}"
        );
        assert!(
            overflow_text.contains("context window"),
            "overflow Display names the window, got {overflow_text:?}"
        );
        assert_ne!(
            (exhausted_status, exhausted_text),
            (overflow_status, overflow_text)
        );
    }

    /// NEX-SEC-003: a step recorded over secret-bearing tool I/O must
    /// persist scrubbed text — the `sk-...` sentinel in both the call
    /// arguments and the observation never reaches `SQLite`, and the live
    /// `StepRecorded` event carries the same sanitized payloads (no secret
    /// crosses IPC either).
    #[test]
    fn tool_call_steps_persist_redacted_not_raw() {
        const SENTINEL_ARGS: &str = "sk-ant-persisted-args-sentinel-0123456789";
        const SENTINEL_OBS: &str = "sk-ant-persisted-obs-sentinel-9876543210";
        const SENTINEL_BEARER: &str = "persistedbearertoken01";
        let db = in_memory_database();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut record =
            ActiveRunRecord::start(RunRecorder::new(&db).with_events(&tx), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        let call = ToolCall {
            id: "c1".to_string(),
            name: "read_file".to_string(),
            arguments: format!(r#"{{"path": "keys.txt", "key": "{SENTINEL_ARGS}"}}"#),
            thought_signature: None,
        };
        let observation =
            format!("file body with {SENTINEL_OBS}\nAuthorization: Bearer {SENTINEL_BEARER}");
        record.tool_call(&call, &observation, "succeeded", Some(5));

        let runs = AgentRunRepository::new(&db);
        let steps = runs.list_steps(run_id).expect("list steps");
        assert_eq!(steps.len(), 1);
        let stored_args = steps[0].arguments.as_deref().unwrap_or("");
        let stored_obs = steps[0].observation.as_deref().unwrap_or("");
        for sentinel in [SENTINEL_ARGS, SENTINEL_OBS, SENTINEL_BEARER] {
            assert!(
                !stored_args.contains(sentinel) && !stored_obs.contains(sentinel),
                "sentinel must not persist: {sentinel:?}"
            );
        }
        assert!(stored_obs.contains("[redacted]"));
        // The live event carries the same sanitized payloads, not the raw I/O.
        match rx.try_recv().expect("step emits") {
            AgentRunEvent::StepRecorded {
                arguments,
                observation,
                ..
            } => {
                assert_eq!(arguments.as_deref(), Some(stored_args));
                assert_eq!(observation.as_deref(), Some(stored_obs));
            }
            other => panic!("expected StepRecorded, got: {other:?}"),
        }
    }

    /// NEX-SEC-003: an `env`-dump observation carries no token-prefix /
    /// scheme / PEM shape, only `FIELD=value` lines — the assignment pass
    /// must still keep the values out of the stored row and the live event.
    #[test]
    fn tool_call_steps_persist_env_dump_redacted_not_raw() {
        const SENTINEL_AWS: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        const SENTINEL_PASS: &str = "hunter2-hunter2";
        let db = in_memory_database();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut record =
            ActiveRunRecord::start(RunRecorder::new(&db).with_events(&tx), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        let call = ToolCall {
            id: "c1".to_string(),
            name: "execute_command".to_string(),
            arguments: r#"{"command": "env"}"#.to_string(),
            thought_signature: None,
        };
        let observation = format!(
            "PATH=/usr/bin:/bin\nAWS_SECRET_ACCESS_KEY={SENTINEL_AWS}\n\
             config {{\"password\": \"{SENTINEL_PASS}\"}}\nHOME=/root"
        );
        record.tool_call(&call, &observation, "succeeded", Some(5));

        let runs = AgentRunRepository::new(&db);
        let steps = runs.list_steps(run_id).expect("list steps");
        assert_eq!(steps.len(), 1);
        let stored_obs = steps[0].observation.as_deref().unwrap_or("");
        for sentinel in [SENTINEL_AWS, SENTINEL_PASS] {
            assert!(
                !stored_obs.contains(sentinel),
                "assigned value must not persist: {sentinel:?}"
            );
        }
        assert!(stored_obs.contains("[redacted]"));
        assert!(stored_obs.contains("HOME=/root"), "benign lines survive");
        // The live event carries the same sanitized payload, not the raw I/O.
        match rx.try_recv().expect("step emits") {
            AgentRunEvent::StepRecorded { observation, .. } => {
                assert_eq!(observation.as_deref(), Some(stored_obs));
            }
            other => panic!("expected StepRecorded, got: {other:?}"),
        }
    }

    /// NEX-SEC-003: an oversized observation is cut at
    /// [`MAX_PERSISTED_STEP_TEXT_CHARS`] with an honest marker naming the
    /// omitted and total counts.
    #[test]
    fn oversized_observation_truncates_with_honest_marker() {
        let db = in_memory_database();
        let mut record = ActiveRunRecord::start(RunRecorder::new(&db), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        let total = MAX_PERSISTED_STEP_TEXT_CHARS + 1_000;
        let observation = "o".repeat(total);
        let call = ToolCall {
            id: "c1".to_string(),
            name: "execute_command".to_string(),
            arguments: r#"{"command": "env"}"#.to_string(),
            thought_signature: None,
        };
        record.tool_call(&call, &observation, "succeeded", None);

        let runs = AgentRunRepository::new(&db);
        let steps = runs.list_steps(run_id).expect("list steps");
        let stored = steps[0].observation.as_deref().expect("observation stored");
        let marker = format!("[truncated 1000 of {total} chars]");
        assert!(
            stored.ends_with(&marker),
            "honest marker required, got tail: {:?}",
            stored.chars().rev().take(60).collect::<String>()
        );
        let kept: String = stored.chars().take(MAX_PERSISTED_STEP_TEXT_CHARS).collect();
        assert_eq!(kept, observation[..MAX_PERSISTED_STEP_TEXT_CHARS]);
    }

    /// NEX-PERF-003: a large observation streams as a bounded preview with a
    /// byte-exact total and an honest marker — while the row keeps the full
    /// sanitized text and the event names the row for the on-demand fetch.
    #[test]
    fn step_recorded_carries_preview_plus_byte_exact_length() {
        let db = in_memory_database();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut record =
            ActiveRunRecord::start(RunRecorder::new(&db).with_events(&tx), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        let total_chars = MAX_STEP_PREVIEW_CHARS + 1_000;
        let observation = "o".repeat(total_chars);
        let call = ToolCall {
            id: "c1".to_string(),
            name: "execute_command".to_string(),
            arguments: r#"{"command": "env"}"#.to_string(),
            thought_signature: None,
        };
        record.tool_call(&call, &observation, "succeeded", None);

        // The row keeps the full sanitized text (short of the 32K persist
        // cap, so byte-identical here).
        let runs = AgentRunRepository::new(&db);
        let steps = runs.list_steps(run_id).expect("list steps");
        assert_eq!(steps.len(), 1);
        let stored = steps[0].observation.as_deref().expect("stored");
        assert_eq!(stored, observation);

        // The live event carries the row id, a bounded preview, the
        // byte-exact total, and the truncation flag — never the full text.
        match rx.try_recv().expect("step emits") {
            AgentRunEvent::StepRecorded {
                step_id,
                run_id: event_run_id,
                seq,
                observation: preview,
                observation_len,
                observation_truncated,
                ..
            } => {
                assert_eq!(step_id, steps[0].id, "event names the stored row");
                assert_eq!(event_run_id, run_id);
                assert_eq!(seq, 1);
                let preview = preview.expect("preview present");
                assert_eq!(
                    observation_len,
                    Some(stored.len() as u64),
                    "total is byte-exact"
                );
                assert!(observation_truncated);
                assert!(preview.len() < stored.len(), "preview must bound the frame");
                assert!(
                    preview.chars().count() <= MAX_STEP_PREVIEW_CHARS + 64,
                    "preview stays near the bound, got {} chars",
                    preview.chars().count()
                );
                let marker = format!("[truncated 1000 of {total_chars} bytes]");
                assert!(
                    preview.ends_with(&marker),
                    "honest marker in the persist-cap vocabulary, got tail: {:?}",
                    preview.chars().rev().take(60).collect::<String>()
                );
            }
            other => panic!("expected StepRecorded, got: {other:?}"),
        }
    }

    /// NEX-PERF-003: short step text streams byte-identical (preview == full,
    /// not truncated, total == bytes) — the common case is untouched.
    #[test]
    fn short_observation_streams_verbatim_with_exact_length() {
        let db = in_memory_database();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut record =
            ActiveRunRecord::start(RunRecorder::new(&db).with_events(&tx), "m", "supervised");
        record.model_turn("thinking out loud", Some(7), None);
        match rx.try_recv().expect("step emits") {
            AgentRunEvent::StepRecorded {
                observation,
                observation_len,
                observation_truncated,
                ..
            } => {
                assert_eq!(observation.as_deref(), Some("thinking out loud"));
                assert_eq!(observation_len, Some("thinking out loud".len() as u64));
                assert!(!observation_truncated);
            }
            other => panic!("expected StepRecorded, got: {other:?}"),
        }
    }

    /// NEX-PERF-003 + NEX-SEC-003: the preview is derived from the
    /// *sanitized* text — a redacted observation previews the redaction,
    /// and the total counts the sanitized bytes, so no secret crosses IPC
    /// even in preview form.
    #[test]
    fn preview_is_derived_from_sanitized_text_never_raw() {
        const SENTINEL: &str = "sk-ant-preview-sentinel-0123456789abcdef";
        let db = in_memory_database();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut record =
            ActiveRunRecord::start(RunRecorder::new(&db).with_events(&tx), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        let call = ToolCall {
            id: "c1".to_string(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "keys.txt"}"#.to_string(),
            thought_signature: None,
        };
        // Long enough to force a preview cut, with the sentinel past the cut
        // so a pre-redaction preview would leak it.
        let observation = format!("{}{SENTINEL}", "p".repeat(MAX_STEP_PREVIEW_CHARS));
        record.tool_call(&call, &observation, "succeeded", None);

        let runs = AgentRunRepository::new(&db);
        let stored = runs.list_steps(run_id).expect("steps")[0]
            .observation
            .clone()
            .expect("stored");
        assert!(!stored.contains(SENTINEL), "stored text is sanitized first");
        match rx.try_recv().expect("step emits") {
            AgentRunEvent::StepRecorded {
                observation,
                observation_len,
                ..
            } => {
                let preview = observation.expect("preview present");
                assert!(
                    !preview.contains(SENTINEL),
                    "preview must not leak pre-redaction text"
                );
                assert_eq!(observation_len, Some(stored.len() as u64));
                assert!(
                    stored.starts_with(&preview[..preview.len().min(stored.len())])
                        || preview.len() <= stored.len() + 64,
                    "preview derives from the stored text"
                );
            }
            other => panic!("expected StepRecorded, got: {other:?}"),
        }
    }

    /// NEX-PERF-003: `step_preview` never splits UTF-8 and reports the marker
    /// in bytes — the same unit as `observation_len` and the UI button.
    #[test]
    fn step_preview_keeps_char_boundaries_and_marker_vocabulary() {
        let text = "é".repeat(MAX_STEP_PREVIEW_CHARS + 10);
        let (preview, total_bytes, truncated) = step_preview(&text);
        assert!(truncated);
        assert_eq!(total_bytes, text.len() as u64, "bytes, not chars");
        assert!(
            preview.starts_with(&"é".repeat(MAX_STEP_PREVIEW_CHARS)),
            "cut lands on a char boundary"
        );
        // "é" is 2 bytes: 2000 kept chars = 4000 kept bytes, 10 omitted
        // chars = 20 omitted bytes, 4020 total bytes.
        let total = text.len() as u64;
        let omitted = total - "é".repeat(MAX_STEP_PREVIEW_CHARS).len() as u64;
        assert!(
            preview.ends_with(&format!("[truncated {omitted} of {total} bytes]")),
            "byte-unit marker, got tail: {:?}",
            preview.chars().rev().take(60).collect::<String>()
        );
        // Short text round-trips byte-identical.
        let (same, len, cut) = step_preview("plain");
        assert_eq!(same, "plain");
        assert_eq!(len, 5);
        assert!(!cut);
    }

    /// NEX-SEC-003 negative control: short benign step text round-trips
    /// byte-identical — redaction and the bound only fire where they must.
    #[test]
    fn benign_step_text_survives_persistence_verbatim() {
        let db = in_memory_database();
        let mut record = ActiveRunRecord::start(RunRecorder::new(&db), "m", "supervised");
        let run_id = record.run_id.expect("run row persisted at start");

        let call = ToolCall {
            id: "c1".to_string(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "notes.txt"}"#.to_string(),
            thought_signature: None,
        };
        record.tool_call(&call, "deploy risk-free task-setup done", "succeeded", None);

        let runs = AgentRunRepository::new(&db);
        let steps = runs.list_steps(run_id).expect("list steps");
        assert_eq!(
            steps[0].arguments.as_deref(),
            Some(r#"{"path": "notes.txt"}"#)
        );
        assert_eq!(
            steps[0].observation.as_deref(),
            Some("deploy risk-free task-setup done")
        );
    }
}
