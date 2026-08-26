//! Application layer: state, workflow, coordination, validation
//! (ARCHITECTURE.md §5).
//!
//! Services here orchestrate the infrastructure layer (repositories) and
//! expose application-facing behavior, including
//! [`settings::SettingsService`], [`providers::ProviderService`],
//! [`execution::RequestExecutionService`],
//! [`conversations::ConversationService`] (ROADMAP.md Phase 4 — Conversations),
//! [`prompts::PromptLibraryService`] (ROADMAP.md Phase 5 — Prompt Library),
//! [`attachments::AttachmentService`] (ROADMAP.md Phase 6 — Documents),
//! [`search::LocalSearchService`] (ROADMAP.md Phase 7 — Local Search),
//! [`export::ExportService`] (ROADMAP.md Phase 8.1 — Conversation Export), and
//! [`import::ImportService`] (ROADMAP.md Phase 8.2 — Conversation Import).

// This crate has no application-layer consumer yet (Tauri commands arrive in
// later tasks), so service items not yet referenced are intentionally unused.
// Remove this attribute once a consumer references a service.
#![allow(dead_code)]

pub mod agent;
pub mod attachments;
pub mod conversations;
pub mod data_management;
pub mod execution;
pub mod export;
pub mod import;
pub mod prompts;
pub mod providers;
pub mod search;
pub mod settings;
