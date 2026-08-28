//! Agent workspace: tools and execution loop.
//!
//! - Task 2 — Core Workspace Tools ([`tools`]): self-contained safe tool
//!   execution for the autonomous agent. Exposes the four native workspace
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

pub mod approval;
pub mod control;
pub mod persistence;
pub mod runner;
pub mod tools;
