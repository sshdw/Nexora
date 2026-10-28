//! Agent run repository: desktop adapter over `nexora_core` (issue #161).
//!
//! The records and SQL live in `nexora_core::storage::repository::agent_runs`
//! so the CLI can read/write the same rows. This module preserves the original
//! desktop path (`infrastructure::repository::agent_runs`).

pub(crate) use nexora_core::storage::repository::agent_runs::*;
