//! Common repository infrastructure (ROADMAP.md Phase 1 — Database &
//! Persistence).
//!
//! Shared, reusable building blocks that every concrete repository
//! (Settings, Conversations, Messages, Prompts, ...) will use so that
//! accessing the single shared connection and running transactions is never
//! duplicated.
//!
//! - [`Result`] centralizes error propagation through [`DatabaseError`].
//! - [`Repository`] is the common abstraction over the shared [`Database`]:
//!   a concrete repository provides its [`Database`] handle and inherits
//!   [`Repository::conn`] and [`Repository::transaction`].
//!
//! Concrete repositories in this module are the desktop-owned tables
//! (conversations, messages, prompts, settings, ...). The foundation and the
//! agent run/task repositories come from `nexora_core::storage::repository`
//! (issue #161) and are re-exported below under their original paths.

// A few desktop-only repository helpers have no in-tree caller yet; this
// module-level allowance predates the extraction and is kept deliberately.
#![allow(dead_code)]

pub(crate) use nexora_core::storage::repository::{Repository, Result};

pub mod agent_runs;
pub mod agent_tasks;
pub mod attachments;
pub mod conversations;
pub mod debt_items;
pub mod messages;
pub mod prompts;
pub mod providers;
pub mod search;
pub mod settings;
