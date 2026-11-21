//! Repository for the shared `app_settings` key/value table.
//!
//! This is the same table the desktop settings service reads and writes, so
//! `nexora config get/set` operates on the exact same persisted settings as
//! Nexora Desktop (brief §3.2, §14). Secrets are not reachable here: the write
//! gate in [`crate::config`] rejects credential-shaped keys before a value can
//! ever reach this repository, and provider credentials live in the OS keyring.

use rusqlite::OptionalExtension;

use crate::storage::database::Database;
use crate::storage::repository::{Repository, Result};

/// Repository over the shared `app_settings` table.
pub struct SettingsRepository<'a> {
    db: &'a Database,
}

impl<'a> SettingsRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    #[must_use]
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for SettingsRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl SettingsRepository<'_> {
    /// Read one setting, returning `None` when it has never been set **or**
    /// when the stored value is SQL `NULL`.
    ///
    /// Desktop writes `NULL` to disconnect a provider (`SettingValue::Null`),
    /// and reads it back as absent; decoding as `Option<String>` and flattening
    /// makes the CLI agree. `OptionalExtension` alone only handles "no rows".
    ///
    /// Uses the read-only pool connection so a `config get` runs concurrently
    /// with a Desktop writer (WAL), exactly like the other read views.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::storage::database::DatabaseError`] if the connection
    /// cannot be acquired or the query fails.
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.read_conn()?;
        conn.query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            [key],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(Into::into)
    }

    /// Insert or replace one setting value.
    ///
    /// Callers must run the shared [`crate::config::validate_setting`] gate
    /// first; this repository only persists. No credential value is ever
    /// passed here: the allowlist has no key that maps to a secret.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::storage::database::DatabaseError`] if the connection
    /// cannot be acquired or the upsert violates a table constraint.
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// Remove one setting, restoring its documented default state.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::storage::database::DatabaseError`] if the connection
    /// cannot be acquired or the delete fails.
    pub fn clear(&self, key: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM app_settings WHERE key = ?1", [key])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::in_memory_database;

    #[test]
    fn get_set_round_trip_and_clear() {
        let db = in_memory_database();
        let repo = SettingsRepository::new(&db);
        assert_eq!(repo.get("appearance.theme").expect("get"), None);
        repo.set("appearance.theme", "dark").expect("set");
        assert_eq!(
            repo.get("appearance.theme").expect("get").as_deref(),
            Some("dark")
        );
        repo.set("appearance.theme", "light").expect("upsert");
        assert_eq!(
            repo.get("appearance.theme").expect("get").as_deref(),
            Some("light")
        );
        repo.clear("appearance.theme").expect("clear");
        assert_eq!(repo.get("appearance.theme").expect("get"), None);
    }

    #[test]
    fn null_value_is_absent_but_the_row_still_exists() {
        let db = in_memory_database();
        let repo = SettingsRepository::new(&db);
        {
            let conn = repo.conn().expect("write conn");
            conn.execute(
                "INSERT INTO app_settings (key, value) VALUES (?1, NULL)",
                ["selected_provider"],
            )
            .expect("raw NULL insert must be storable");
        } // release the write guard before reading

        assert_eq!(
            repo.get("selected_provider").expect("get"),
            None,
            "Desktop's NULL disconnect marker must read as absent"
        );

        let conn = repo.read_conn().expect("read conn");
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE key = ?1",
                ["selected_provider"],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(rows, 1, "NULL is a stored row, not a missing key");
    }
}
