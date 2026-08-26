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

pub mod runner;
pub mod tools;
