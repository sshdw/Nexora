//! Agent workspace: tools and execution loop.
//!
//! - Task 2 — Core Workspace Tools ([`tools`]): self-contained safe tool
//!   execution for the autonomous agent. Exposes the six native workspace
//!   tools via [`ToolRegistry`]; intentionally isolated from the conversation
//!   and database layers.
//! - Task 3.1 — Agent Runner ([`runner`]): the deterministic `ReAct` loop
//!   (`runner::AgentRunner`) that drives a provider executor together with the
//!   [`ToolRegistry`] until the model produces final text or the iteration
//!   budget is exhausted.
//! - Task 3.2 — Step Governor & Cancellation ([`control`]): adaptive step
//!   budgets, user pause/resume, instant cancellation
//!   (`control::RunControl`), and the governance event channel
//!   (`control::AgentRunEvent`) wrapped around the runner loop.
//! - Task 4.1 — Three-Tier Approval Gate ([`approval`]): autonomy ladder
//!   (`approval::ApprovalGate`) that decides per tool risk class and
//!   [`approval::AutonomyMode`] whether a call executes automatically or
//!   parks until the user approves or denies it.
//! - Task 4.2 — Agent Run Persistence ([`persistence`]): the opt-in run
//!   recorder (`persistence::RunRecorder`) that persists one `agent_runs`
//!   row and append-only `agent_steps` rows (DATABASE.md §7.8, §7.9) when —
//!   and only when — it is attached to the runner; without a recorder the
//!   loop keeps the exact pre-4.2 behaviour.
//! - Task 5.1 — Run Bridge ([`service`]): spawns runs on dedicated threads,
//!   streams every governance/step event to the frontend as `agent-run-event`
//!   frames, tracks active runs (`service::AgentRunRegistry`), and links runs
//!   to conversations (D50).
//! - WS-B.2 — Budgets, Gates, Audit ([`governance`]): the enforceable
//!   [`governance::RunBudget`] resolved from the lifecycle [`lifecycle`]
//!   `BudgetHandles` contract, [`governance::GateOutcome`] checkpoints reusing
//!   the #65 ladder unchanged, and the append-only in-memory
//!   [`governance::AuditLog`] accessory to the persistence row (no new table,
//!   secret-free by construction).
//! - WS-B.3 — Roles & Pipeline ([`roles`], [`pipeline`]): six hardcoded
//!   [`roles::AgentRole`]s pinning tool subsets, task-key defaults, and
//!   approval-posture references (naming the #65 `RiskClass` outcome, never
//!   remapping it), plus the ordered [`pipeline::Pipeline`] stage list with
//!   transition-gated advances, parent-capped budget slices, and stage
//!   entries on the [`governance::AuditLog`].
//! - Task T4 — Context Compaction ([`compaction`]): pure threshold-triggered
//!   and overflow-driven folding of older in-run turns into a summary, with
//!   a verbatim retained tail (`compaction::ContextGovernor`).
//! - Smart Context Assembly ([`assembly`]): stage-aware, priority-ordered
//!   section budgets over the canonical model window with oldest-first drops
//!   and a once-only proactive hook into the existing compaction path.
//! - WS-C.1 — Snapshots & Checkpoints ([`snapshots`]): in-memory run-scoped
//!   position markers with monotonic-budget rollback and named checkpoints,
//!   audited on the trail with fixed vocabulary (no new table).
//! - WS-C.2 — Injection Hardening ([`injection`]): fenced untrusted-output
//!   envelopes plus the pinned marker scan backing the Reviewer checklist,
//!   with hits parked through the existing approval gate.
//! - WS-C.3 — Self-Audit ([`self_audit`]): a read-only pass over the run's
//!   own trail (audit log + snapshots + recorded verdicts) that fails closed
//!   with fixed-vocabulary codes; opt-in, so runs without it behave
//!   byte-identically.
//! - WS-D.1 — Run Inspector ([`inspect`]): one read-only aggregate view over
//!   the WS-B/WS-C accessories (state, stage + role, budget counters, gate
//!   decisions, snapshot/checkpoint markers, self-audit verdict); `&`-borrows
//!   only, secret-free by construction, zero new state.
//! - Spend Dashboard ([`spend`]): one read-only spend view over the persisted
//!   budget counters (per-run steps/micro-USD/caps/ratios plus cross-run
//!   totals); `&`-borrows only, secret-free by construction, zero new state.

pub mod action_memory;
pub mod approval;
pub mod assembly;
pub mod budget;
pub mod compaction;
pub mod control;
pub mod dispatch;
#[cfg(test)]
mod e2e;
pub mod errors;
pub mod governance;
pub mod history;
pub mod injection;
pub mod inspect;
pub mod lifecycle;
pub mod permissions;
pub mod persistence;
pub mod pipeline;
pub mod pricing;
pub mod prompts;
pub mod registry;
pub mod roles;
pub mod runner;
pub mod self_audit;
pub mod service;
pub mod snapshots;
pub mod spend;
#[cfg(test)]
mod stress;
pub mod tools;
