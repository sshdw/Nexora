//! Provider catalog — now owned by `nexora-core` (CLI stream, issue #162).
//!
//! The hardcoded provider identities, model lists, rate tables and context
//! windows were extracted verbatim into [`nexora_core::catalog`] so the Desktop
//! and the CLI describe the same provider surface from the same data. This
//! module is a thin `pub(crate)` re-export: every existing consumer path
//! (`openai.rs`, `gemini.rs`, `registry`, `context_stats`, `privacy`, commands)
//! keeps compiling unchanged, and there is no second registry to drift.

pub(crate) use nexora_core::catalog::{gemini, openai};
