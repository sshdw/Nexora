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

pub mod approval;
pub mod control;
pub mod runner;
pub mod tools;
