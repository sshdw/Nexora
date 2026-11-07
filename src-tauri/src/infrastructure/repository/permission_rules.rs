//! Permission-rules repository: persistence for the `permission_rules` table
//! (migration v7, M1-core; DATABASE.md schema).
//!
//! The table stores ordered, revocable tool-permission rules
//! (`preset`, `tool_pattern`, `path_pattern`, `effect`, `priority`). This
//! repository is the only data-access path for that table and reuses the
//! [`Repository`] foundation, so it never duplicates connection handling.
//!
//! It is responsible **only** for persistence: row shape, column vocabulary,
//! and ordering. Rule matching, specificity, mode overrides, and the
//! group-scope confirmation policy live in the application layer
//! ([`crate::application::agent::permissions`]), which is this table's only
//! consumer — the desktop agent owns the table, so unlike
//! `agent_runs`/`agent_tasks` the repository itself stays in the desktop
//! adapter rather than `nexora-core::storage::repository` (nothing in
//! `apps/nexora-cli` reads these rows).

use rusqlite::params;
use serde::Serialize;

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::Repository;

/// Effect of a permission rule (the `effect` column vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuleEffect {
    /// Auto-authorize (subject to mode overrides in the application layer).
    Allow,
    /// Always park for an explicit decision.
    Ask,
    /// Always deny.
    Deny,
}

impl RuleEffect {
    /// Parse the column value.
    fn from_column(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }

    /// Column value.
    #[must_use]
    pub(crate) fn as_column(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// One `permission_rules` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PermissionRule {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Run preset the rule applies to (`coding` / `document` / `*`).
    pub preset: String,
    /// Tool name pattern (`read_file` / `*`).
    pub tool_pattern: String,
    /// Path pattern (`NULL`, a directory, or `*` for match-any).
    pub path_pattern: Option<String>,
    /// Rule effect.
    pub effect: RuleEffect,
    /// Ordering priority (`ASC`; lower wins within one specificity level).
    pub priority: i64,
}

/// Repository for the `permission_rules` table.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle. The
/// application layer ([`crate::application::agent::permissions`]) wraps these
/// calls; the IPC commands never touch this module directly.
pub(crate) struct PermissionRulesRepository<'a> {
    db: &'a Database,
}

impl<'a> PermissionRulesRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// List all rules ordered `priority ASC, id ASC`.
    pub(crate) fn list(&self) -> std::result::Result<Vec<PermissionRule>, DatabaseError> {
        let conn = self.read_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, preset, tool_pattern, path_pattern, effect, priority \
             FROM permission_rules ORDER BY priority ASC, id ASC",
        )?;
        let rows = stmt.query_map([], row_to_rule)?;
        let mut rules = Vec::new();
        for row in rows {
            rules.push(row?);
        }
        Ok(rules)
    }

    /// Insert one rule row. Returns the new id.
    pub(crate) fn insert(
        &self,
        preset: &str,
        tool_pattern: &str,
        path_pattern: Option<&str>,
        effect: RuleEffect,
        priority: i64,
    ) -> std::result::Result<i64, DatabaseError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO permission_rules (preset, tool_pattern, path_pattern, effect, priority) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                preset,
                tool_pattern,
                path_pattern,
                effect.as_column(),
                priority
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Delete one rule row. Returns `true` when a row was removed.
    pub(crate) fn delete(&self, id: i64) -> std::result::Result<bool, DatabaseError> {
        let conn = self.conn()?;
        let removed = conn.execute("DELETE FROM permission_rules WHERE id = ?1", params![id])?;
        Ok(removed > 0)
    }
}

impl Repository for PermissionRulesRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

/// Decode one `permission_rules` row.
fn row_to_rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<PermissionRule> {
    let effect_text: String = row.get(4)?;
    let effect = RuleEffect::from_column(&effect_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("unknown effect '{effect_text}'").into(),
        )
    })?;
    Ok(PermissionRule {
        id: row.get(0)?,
        preset: row.get(1)?,
        tool_pattern: row.get(2)?,
        path_pattern: row.get(3)?,
        effect,
        priority: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    #[test]
    fn insert_list_delete_round_trip_is_ordered_and_typed() {
        let db = in_memory_database();
        let repo = PermissionRulesRepository::new(&db);
        // The v7 migration already seeded one ask row for the shell
        // (`('coding', 'execute_command', '*', 'ask', 50)`).
        repo.insert("coding", "*", None, RuleEffect::Ask, 0)
            .expect("insert wildcard ask");
        repo.insert("coding", "write_file", Some("docs"), RuleEffect::Allow, 0)
            .expect("insert allow");
        repo.insert("coding", "read_file", Some("src"), RuleEffect::Deny, 100)
            .expect("insert deny");

        let rules = repo.list().expect("list rules");
        assert_eq!(rules.len(), 4, "v7 seed row plus the three inserts");
        // priority ASC, id ASC: the two priority-0 inserts keep insertion
        // order, the priority-50 seed follows, the priority-100 row is last.
        assert_eq!(rules[0].tool_pattern, "*");
        assert_eq!(rules[0].effect, RuleEffect::Ask);
        assert_eq!(rules[1].tool_pattern, "write_file");
        assert_eq!(rules[1].effect, RuleEffect::Allow);
        assert_eq!(rules[1].path_pattern.as_deref(), Some("docs"));
        assert_eq!(rules[2].tool_pattern, "execute_command");
        assert_eq!(rules[2].effect, RuleEffect::Ask);
        assert_eq!(rules[2].priority, 50);
        assert_eq!(rules[3].tool_pattern, "read_file");
        assert_eq!(rules[3].effect, RuleEffect::Deny);
        assert_eq!(rules[3].priority, 100);

        let id = rules[3].id;
        assert!(repo.delete(id).expect("delete existing"));
        assert!(!repo.delete(id).expect("delete missing"));
        assert_eq!(repo.list().expect("list after delete").len(), 3);
    }

    #[test]
    fn unknown_effect_values_are_rejected_by_the_schema() {
        // The `effect` column carries a CHECK constraint, so an unknown
        // effect value can never be stored in the first place — the
        // `row_to_rule` decode fallback (a `FromSqlConversionFailure`) stays
        // as defence in depth but is unreachable through this repository on
        // the real schema. This pins the guarantee that matters: writes of
        // unknown vocabulary fail closed.
        let db = in_memory_database();
        let repo = PermissionRulesRepository::new(&db);
        repo.insert("coding", "read_file", None, RuleEffect::Ask, 0)
            .expect("insert");
        let rejected = {
            let conn = db.lock().expect("database lock");
            conn.execute(
                "UPDATE permission_rules SET effect = 'maybe' WHERE tool_pattern = 'read_file'",
                [],
            )
        };
        assert!(
            rejected.is_err(),
            "the effect CHECK constraint must reject unknown vocabulary"
        );
        assert_eq!(repo.list().expect("rows untouched").len(), 2);
    }
}
