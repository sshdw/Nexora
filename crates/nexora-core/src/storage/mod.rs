//! Shared storage: one database bootstrap, one migration set, one app-data
//! location, one repository foundation (issue #161, phase 2 of the CLI
//! stream).
//!
//! The desktop adapter reaches these rules through its original
//! `infrastructure::database` / `infrastructure::repository` paths (thin
//! re-exports); the CLI links this module directly. Nothing here depends on
//! Tauri, React, Ratatui or Clap.
//!
//! Cross-process rules (issue #161):
//!
//! - [`database::open`] takes a migration-leadership lock beside the file, so
//!   exactly one process migrates; later opens wait up to
//!   [`database::BUSY_TIMEOUT_MS`], then read the migrated schema, and fail
//!   honestly with [`database::DatabaseError::Busy`] past the budget;
//! - [`lifecycle::InstanceLease`] + [`lifecycle::sweep_orphans`] make the
//!   startup orphan sweep safe while another Nexora process is alive.

pub mod app_data;
pub mod database;
pub mod lifecycle;
pub mod repository;

pub use database::{
    apply_migration, configure, in_memory_database, migrate, open, target_version, Database,
    DatabaseError, BUSY_TIMEOUT_MS, MIGRATIONS,
};
pub use repository::{Repository, Result};
