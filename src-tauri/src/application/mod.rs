//! Application layer: state, workflow, coordination, validation
//! (ARCHITECTURE.md §5).
//!
//! Services here orchestrate the infrastructure layer (repositories) and
//! expose application-facing behavior, including
//! [`settings::SettingsService`], [`providers::ProviderService`],
//! [`execution::RequestExecutionService`],
//! [`conversations::ConversationService`] (ROADMAP.md Phase 4 — Conversations),
//! and [`prompts::PromptLibraryService`] (ROADMAP.md Phase 5 — Prompt Library).

// This crate has no application-layer consumer yet (Tauri commands arrive in
// later tasks), so service items not yet referenced are intentionally unused.
// Remove this attribute once a consumer references a service.
#![allow(dead_code)]

pub mod conversations;
pub mod execution;
pub mod prompts;
pub mod providers;
pub mod settings;
