//! Tauri command layer (Phase 10.2 — Tauri Command Layer).
//!
//! This module translates Tauri IPC inputs/outputs to and from the existing
//! application-layer services and infrastructure. Its responsibilities are
//! deliberately narrow (ARCHITECTURE.md §5):
//!
//! - declare `#[tauri::command]` functions over the existing services;
//! - convert classified application errors into safe, serializable
//!   [`error::CommandError`] values without leaking credentials or secrets;
//! - provide no business logic, no repository access, and no new behavior.
//!
//! The commands are wired into the application through
//! `tauri::Builder::invoke_handler` in [`crate::run`].

pub mod attachments;
pub mod conversations;
pub mod credentials;
pub mod data_management;
pub mod error;
pub mod import_export;
pub mod prompts;
pub mod providers;
pub mod search;
pub mod settings;
