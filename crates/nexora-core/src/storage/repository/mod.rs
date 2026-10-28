//! Common repository infrastructure (ROADMAP.md Phase 1 — Database &
//! Persistence), shared by every shell.
//!
//! - [`Result`] centralizes error propagation through [`DatabaseError`].
//! - [`Repository`] is the common abstraction over the shared [`Database`]:
//!   a concrete repository provides its [`Database`] handle and inherits
//!   [`Repository::conn`] and [`Repository::transaction`].
//!
//! The desktop adapter re-exports this foundation under its original
//! `infrastructure::repository` path; the CLI links the same code directly.

use rusqlite::{Connection, Transaction};

use super::database::{Database, DatabaseError};

pub mod agent_runs;
pub mod agent_tasks;

/// Result type shared by all repositories, centralizing propagation of
/// [`DatabaseError`].
pub type Result<T> = std::result::Result<T, DatabaseError>;

/// Common abstraction for repositories backed by the shared database.
///
/// A concrete repository implements [`Repository`], returning the shared
/// [`Database`] from [`Repository::db`], and gains connection and transaction
/// handling without repeating boilerplate.
pub trait Repository {
    /// The shared application database this repository persists to. The
    /// implementation returns the managed [`Database`] instance.
    fn db(&self) -> &Database;

    /// Acquire the single shared connection.
    ///
    /// Returns a guard providing exclusive access to the connection.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the connection lock is poisoned.
    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.db().lock()
    }

    /// Run `body` inside a transaction on the shared connection.
    ///
    /// Commits on success; on failure rolls back and returns the error.
    ///
    /// The transaction holds the single connection's lock, so concurrent
    /// access is serialized; keep each transaction short.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the lock cannot be acquired or the
    /// transaction cannot be started or committed, or if the closure returns
    /// an error.
    fn transaction<T, F>(&self, body: F) -> Result<T>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T>,
    {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let result = body(&tx);
        // Commit on success; on failure the `Transaction` is dropped and rolls
        // back, and the closure's original error is returned unchanged.
        result.and_then(|value| {
            tx.commit()?;
            Ok(value)
        })
    }
}
