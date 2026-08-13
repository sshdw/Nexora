//! Infrastructure layer.
//!
//! Responsible for database, filesystem, AI providers, operating-system
//! integration, and configuration (ARCHITECTURE.md §5 Application Layers).

pub mod database;
pub mod logging;

pub mod repository;
