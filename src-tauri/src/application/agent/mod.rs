//! Agent workspace tools (Task 2 — Core Workspace Tools).
//!
//! Self-contained safe tool execution for the autonomous agent. The module
//! exposes the four native workspace tools via [`ToolRegistry`]. It is
//! intentionally isolated from conversation and database layers.

pub mod tools;
