//! Settings service: application-layer coordination for persistent
//! application settings (SRS §11, FR-012; ROADMAP.md Phase 2).
//!
//! This service sits in the application layer (ARCHITECTURE.md §5) and
//! orchestrates access to [`SettingsRepository`], mapping repository results
//! to application-facing types and resolving defaults where the specification
//! prescribes them. All persistence is delegated to the repository: this
//! service contains no SQL, no string interpolation, and never touches the
//! shared connection directly.
//!
//! FR-012 requires that settings persist across restarts and that changes
//! apply without data loss. SRS §11 defines the configurable setting
//! categories but prescribes no concrete keys or default values, so this
//! service is intentionally generic over `key`/`value` and applies no
//! invented defaults (an absent setting resolves to [`None`]). Value
//! validation is the responsibility of higher application layers and is not
//! enforced here.
//!
//! No credentials, secrets, or tokens are handled here; provider credentials
//! belong exclusively to the OS keyring and are never stored in `SQLite`.

use crate::infrastructure::database::Database;
use crate::infrastructure::repository::settings::{SettingValue, SettingsRepository};
use crate::infrastructure::repository::Result;

/// Application-layer service coordinating persistent application settings.
///
/// Wraps [`SettingsRepository`] and re-exposes its operations through
/// application-facing types. It is deliberately focused on orchestration:
/// default-value resolution and result mapping only, with no business logic.
pub(crate) struct SettingsService<'a> {
    repo: SettingsRepository<'a>,
}

impl<'a> SettingsService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            repo: SettingsRepository::new(db),
        }
    }

    /// Read a setting by `key`.
    ///
    /// Returns the stored value as [`Some`] when a non-`NULL` value exists, or
    /// [`None`] when the setting is absent or stored as `NULL` (the default
    /// state). No concrete FR-012 default is defined by the specification, so
    /// an absent setting resolves to [`None`].
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] on a failed query or a poisoned connection.
    pub(crate) fn read(&self, key: &str) -> Result<Option<String>> {
        match self.repo.read(key)? {
            SettingValue::Value(value) => Ok(Some(value)),
            SettingValue::Missing | SettingValue::Null => Ok(None),
        }
    }

    /// Persist a setting by `key`, inserting it when absent and updating its
    /// value when present.
    ///
    /// `value` may be [`None`] to store a `NULL` value. Writing an existing
    /// key updates it in place (no data loss), satisfying FR-012.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the existence check or the write fails.
    pub(crate) fn write(&self, key: &str, value: Option<&str>) -> Result<()> {
        if self.repo.exists(key)? {
            self.repo.update(key, value)
        } else {
            self.repo.create(key, value)
        }
    }

    /// Delete a setting by `key`.
    ///
    /// Deleting a non-existent `key` is a no-op.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete(&self, key: &str) -> Result<()> {
        self.repo.delete(key)
    }

    /// Read every setting as `(key, value)` pairs.
    ///
    /// Pairs are ordered by `key`.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<(String, Option<String>)>> {
        self.repo.list()
    }
}
