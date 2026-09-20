//! Persistent permission rules (M1-core): ordered, revocable, deny-wins.
//!
//! Rules live in `permission_rules` (migration v7, forward-only) and are
//! evaluated per tool call before the approval ladder (`approval.rs`). Every
//! run is implicitly `"coding"` (M2): rules with `preset IN ('coding','*')`
//! are evaluated; `preset='document'` rows are storable but inert until M2.
//!
//! Matching (normative):
//! 1. Candidates: `preset IN (run_preset,'*')` AND `tool_pattern IN
//!    (tool,'*')` AND (`path_pattern IS NULL` OR request-path equals/is-under
//!    it OR `== '*'`).
//! 2. Sort: (tool-exact+path) > (tool-exact) > (wildcard+path) > (wildcard);
//!    then `priority ASC`, `id ASC`. Equal-specificity + equal-priority ties
//!    prefer Deny.
//! 3. No match: fall back to the autonomy ladder (`ApprovalGate`).
//! 4. Mode overrides (applied by the runner, not the store): `Supervised`
//!    ignores `Allow` rows; `FullAutonomous` still enforces `Deny`, treats
//!    `Ask` as `Allow`. Unknown tools never reach the store (dispatch rejects
//!    them).

use rusqlite::params;
use serde::Serialize;

use crate::application::agent::approval::RiskClass;
use crate::infrastructure::database::{Database, DatabaseError};

/// Effect of a permission rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuleEffect {
    Allow,
    Ask,
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
    pub id: i64,
    pub preset: String,
    pub tool_pattern: String,
    pub path_pattern: Option<String>,
    pub effect: RuleEffect,
    pub priority: i64,
}

/// Raw rule outcome (before mode overrides).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PermissionOutcome {
    Allow {
        rule_id: Option<i64>,
    },
    Ask {
        rule_id: Option<i64>,
    },
    Deny {
        rule_id: Option<i64>,
        reason: String,
    },
}

/// Snapshot of rules evaluated per tool call.
#[derive(Debug, Clone, Default)]
pub(crate) struct PermissionStore {
    rules: Vec<PermissionRule>,
}

impl PermissionStore {
    /// Empty store (no rules): every decision falls back to the ladder.
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self { rules: Vec::new() }
    }

    /// Build from an explicit rule list (test seam).
    #[must_use]
    pub(crate) fn from_rules(rules: Vec<PermissionRule>) -> Self {
        Self { rules }
    }

    /// Load all rules ordered `priority ASC, id ASC` (best-effort for the
    /// runner: a load failure yields an empty store and the ladder governs).
    #[must_use]
    pub(crate) fn load(db: &Database) -> Self {
        match list_rules(db) {
            Ok(rules) => Self { rules },
            Err(err) => {
                log::warn!("permission rules: load failed, continuing without rules: {err}");
                Self { rules: Vec::new() }
            }
        }
    }

    /// Ordered evaluation. Returns `None` when no rule matches (the caller
    /// falls back to `ApprovalGate::needs_approval`).
    #[must_use]
    pub(crate) fn decide(
        &self,
        preset: &str,
        tool: &str,
        path: Option<&str>,
        _risk: RiskClass,
    ) -> Option<PermissionOutcome> {
        let mut candidates: Vec<&PermissionRule> = self
            .rules
            .iter()
            .filter(|rule| rule.preset.as_str() == preset || rule.preset.as_str() == "*")
            .filter(|rule| rule.tool_pattern.as_str() == tool || rule.tool_pattern.as_str() == "*")
            .filter(|rule| path_matches(rule.path_pattern.as_deref(), path))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        // Specificity: tool-exact+path (3) > tool-exact (2) > wildcard+path (1)
        // > wildcard (0). `*` path_pattern counts as no-path.
        candidates.sort_by(|a, b| {
            specificity(b)
                .cmp(&specificity(a))
                .then_with(|| a.priority.cmp(&b.priority))
                .then_with(|| a.id.cmp(&b.id))
        });
        let top_specificity = specificity(candidates[0]);
        let top_priority = candidates[0].priority;
        // Deny-wins: among the top specificity+priority cohort, prefer Deny.
        let cohort: Vec<&&PermissionRule> = candidates
            .iter()
            .filter(|rule| specificity(rule) == top_specificity && rule.priority == top_priority)
            .collect();
        let chosen = if cohort.iter().any(|rule| rule.effect == RuleEffect::Deny) {
            cohort
                .iter()
                .filter(|rule| rule.effect == RuleEffect::Deny)
                .min_by_key(|rule| rule.id)
                .expect("deny exists in cohort")
        } else {
            candidates.first().expect("candidates non-empty")
        };
        let rule_id = Some(chosen.id);
        match chosen.effect {
            RuleEffect::Allow => Some(PermissionOutcome::Allow { rule_id }),
            RuleEffect::Ask => Some(PermissionOutcome::Ask { rule_id }),
            RuleEffect::Deny => Some(PermissionOutcome::Deny {
                rule_id,
                reason: format!("denied by rule:{}", chosen.id),
            }),
        }
    }
}

/// Whether a rule path matches the request path.
fn path_matches(rule_path: Option<&str>, request_path: Option<&str>) -> bool {
    match rule_path {
        None | Some("*") => true,
        Some(pattern) => match request_path {
            None => false,
            Some(request) => {
                let pattern = pattern.trim_end_matches('/');
                let request = request.trim_end_matches('/');
                request == pattern || request.starts_with(&format!("{pattern}/"))
            }
        },
    }
}

/// Specificity level 3..0 (higher wins).
fn specificity(rule: &PermissionRule) -> u8 {
    let tool_exact = rule.tool_pattern.as_str() != "*";
    let has_path = match rule.path_pattern.as_deref() {
        None | Some("*") => false,
        Some(_) => true,
    };
    match (tool_exact, has_path) {
        (true, true) => 3,
        (true, false) => 2,
        (false, true) => 1,
        (false, false) => 0,
    }
}

/// Extract the request path from a tool call's raw JSON arguments.
///
/// File tools (`read_file`, `write_file`) normalise to the parent directory;
/// directory tools (`list_directory` `path`, `execute_command` `cwd`) use the
/// directory itself. Returns `None` for pathless calls (or unparseable args).
#[must_use]
pub(crate) fn extract_path(tool_name: &str, arguments_json: &str) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    match tool_name {
        "read_file" | "write_file" => {
            let path = args.get("path")?.as_str()?;
            if path.trim().is_empty() {
                return None;
            }
            Some(parent_dir(path))
        }
        "list_directory" => {
            let path = args.get("path")?.as_str()?;
            if path.trim().is_empty() {
                return None;
            }
            Some(normalize_dir(path))
        }
        "execute_command" => {
            let cwd = args.get("cwd")?.as_str()?;
            if cwd.trim().is_empty() {
                return None;
            }
            Some(normalize_dir(cwd))
        }
        _ => None,
    }
}

/// Parent directory of a file path (`a/b/c.txt` -> `a/b`; `a.txt` -> `*`).
fn parent_dir(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let trimmed = normalized.trim().trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(index) => {
            let parent = trimmed[..index].trim();
            if parent.is_empty() {
                "*".to_string()
            } else {
                parent.to_string()
            }
        }
        None => "*".to_string(),
    }
}

/// Normalise a directory argument (trim, strip trailing slashes; root -> `*`).
fn normalize_dir(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let trimmed = normalized.trim().trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        "*".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Group key `(preset, tool, parent-dir-or-*)`, encoded as
/// `preset:tool:path`, truncated to 256 chars for the column CHECK.
#[must_use]
pub(crate) fn group_key(preset: &str, tool_name: &str, path: Option<&str>) -> String {
    let path_part = match path {
        None | Some("*" | "") => "*".to_string(),
        Some(p) => {
            let normalized = p.replace('\\', "/");
            let trimmed = normalized.trim().trim_end_matches('/');
            if trimmed.is_empty() {
                "*".to_string()
            } else {
                trimmed.to_string()
            }
        }
    };
    let mut key = format!("{preset}:{tool_name}:{path_part}");
    if key.len() > 256 {
        key.truncate(256);
    }
    key
}

/// Whether a tool name is one of the four native tools (unknown tools never
/// reach the store).
#[must_use]
pub(crate) fn is_known_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "list_directory" | "write_file" | "execute_command"
    )
}

// ---------------------------------------------------------------------------
// Persistence helpers (used by the commands layer + service bridge)
// ---------------------------------------------------------------------------

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

/// List all rules ordered `priority ASC, id ASC`.
pub(crate) fn list_rules(db: &Database) -> Result<Vec<PermissionRule>, DatabaseError> {
    let conn = db.lock()?;
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
pub(crate) fn insert_rule(
    db: &Database,
    preset: &str,
    tool_pattern: &str,
    path_pattern: Option<&str>,
    effect: RuleEffect,
    priority: i64,
) -> Result<i64, DatabaseError> {
    let conn = db.lock()?;
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
pub(crate) fn delete_rule(db: &Database, id: i64) -> Result<bool, DatabaseError> {
    let conn = db.lock()?;
    let removed = conn.execute("DELETE FROM permission_rules WHERE id = ?1", params![id])?;
    Ok(removed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(
        id: i64,
        preset: &str,
        tool: &str,
        path: Option<&str>,
        effect: RuleEffect,
        priority: i64,
    ) -> PermissionRule {
        PermissionRule {
            id,
            preset: preset.to_string(),
            tool_pattern: tool.to_string(),
            path_pattern: path.map(str::to_string),
            effect,
            priority,
        }
    }

    #[test]
    fn decide_specificity_priority_and_deny_floor() {
        // 4-level specificity: exact+path > exact > wildcard+path > wildcard.
        let store = PermissionStore::from_rules(vec![
            rule(1, "coding", "*", None, RuleEffect::Allow, 0),
            rule(2, "coding", "*", Some("src"), RuleEffect::Allow, 0),
            rule(3, "coding", "read_file", None, RuleEffect::Allow, 0),
            rule(4, "coding", "read_file", Some("src"), RuleEffect::Deny, 100),
        ]);
        // Exact+path wins despite lower priority number elsewhere.
        let outcome = store
            .decide("coding", "read_file", Some("src"), RiskClass::ReadOnly)
            .expect("must match");
        assert!(matches!(outcome, PermissionOutcome::Deny { .. }));

        // Exact without path beats wildcard with path.
        let store = PermissionStore::from_rules(vec![
            rule(1, "coding", "*", Some("src"), RuleEffect::Deny, 0),
            rule(2, "coding", "read_file", None, RuleEffect::Allow, 100),
        ]);
        let outcome = store
            .decide(
                "coding",
                "read_file",
                Some("src/file.txt"),
                RiskClass::ReadOnly,
            )
            .expect("must match");
        assert!(matches!(outcome, PermissionOutcome::Allow { .. }));

        // Priority ASC then id ASC within one level.
        let store = PermissionStore::from_rules(vec![
            rule(10, "coding", "read_file", None, RuleEffect::Deny, 200),
            rule(11, "coding", "read_file", None, RuleEffect::Allow, 50),
        ]);
        let outcome = store
            .decide("coding", "read_file", None, RiskClass::ReadOnly)
            .expect("must match");
        assert!(matches!(outcome, PermissionOutcome::Allow { .. }));
        let store = PermissionStore::from_rules(vec![
            rule(10, "coding", "read_file", None, RuleEffect::Allow, 50),
            rule(11, "coding", "read_file", None, RuleEffect::Deny, 50),
        ]);
        // Equal specificity + equal priority: deny wins over id order.
        let outcome = store
            .decide("coding", "read_file", None, RiskClass::ReadOnly)
            .expect("must match");
        assert!(matches!(outcome, PermissionOutcome::Deny { .. }));

        // Preset/tool/path wildcards.
        let store =
            PermissionStore::from_rules(vec![rule(1, "*", "*", Some("*"), RuleEffect::Ask, 100)]);
        assert!(store
            .decide("coding", "write_file", Some("docs"), RiskClass::Mutating)
            .is_some());
        assert!(store
            .decide("document", "write_file", Some("docs"), RiskClass::Mutating)
            .is_some());
        // Document-only rule is inert for coding runs.
        let store = PermissionStore::from_rules(vec![rule(
            1,
            "document",
            "read_file",
            None,
            RuleEffect::Deny,
            0,
        )]);
        assert!(store
            .decide("coding", "read_file", None, RiskClass::ReadOnly)
            .is_none());
        // Path scoping: equals or is-under; '*' and NULL match anything.
        let store = PermissionStore::from_rules(vec![rule(
            1,
            "coding",
            "read_file",
            Some("src"),
            RuleEffect::Deny,
            0,
        )]);
        assert!(store
            .decide("coding", "read_file", Some("src"), RiskClass::ReadOnly)
            .is_some());
        assert!(store
            .decide("coding", "read_file", Some("src/sub"), RiskClass::ReadOnly)
            .is_some());
        assert!(store
            .decide("coding", "read_file", Some("docs"), RiskClass::ReadOnly)
            .is_none());
        assert!(store
            .decide("coding", "read_file", None, RiskClass::ReadOnly)
            .is_none());

        // No match falls back to the ladder (None here; the runner consults
        // the gate).
        let store = PermissionStore::empty();
        assert!(store
            .decide("coding", "read_file", None, RiskClass::ReadOnly)
            .is_none());
    }

    #[test]
    fn removed_rule_falls_back_to_ladder() {
        let db = crate::infrastructure::database::in_memory_database();
        let id =
            insert_rule(&db, "coding", "read_file", None, RuleEffect::Deny, 10).expect("insert");
        let store = PermissionStore::load(&db);
        assert!(
            store
                .decide("coding", "read_file", None, RiskClass::ReadOnly)
                .is_some(),
            "inserted rule must hit"
        );
        assert!(delete_rule(&db, id).expect("delete"));
        let store = PermissionStore::load(&db);
        assert!(
            store
                .decide("coding", "read_file", None, RiskClass::ReadOnly)
                .is_none(),
            "removed rule must fall back to the ladder"
        );
    }
}
