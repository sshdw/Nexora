//! Tauri-free shared backend for Nexora.
//!
//! This crate owns the application services and infrastructure adapters that
//! used to live under `src-tauri/src/{application,infrastructure}`. It links
//! no UI framework, no Tauri runtime, and no CLI argument parser: shells
//! depend on it through the typed seams in `nexora_core` and inject their own
//! async runtime through [`AsyncSpawner`].
//!
//! The layout is deliberately unchanged from the desktop source tree so the
//! extraction is a pure code move with zero Desktop behavior delta.
//!
//! Not every shared service is consumed by every shell; unused items are
//! intentionally retained for the CLI adapters that link this crate.
#![allow(dead_code)]
// The shell-facing API is inherited verbatim from a tree that was entirely
// crate-private in the desktop app. Widening just the items reached by the
// shell triggers pedantic API-documentation lints on code whose docs and
// signatures are unchanged; rewriting them is out of scope for a pure move.
#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]
#![allow(clippy::return_self_not_must_use)]

pub mod application;
pub mod async_spawner;
pub mod command_error_mapping;
pub mod infrastructure;

pub use async_spawner::{AbortHandle, AsyncSpawner};
