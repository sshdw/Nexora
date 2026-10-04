//! Persistent permission rules (M1-core): ordered, revocable, deny-wins.
//!
//! Rules live in `permission_rules` (migration v7, forward-only) and are
//! evaluated per tool call before the approval ladder (`approval.rs`). Every
//! run carries a [`RunPreset`] (`Coding` default): rules with `preset IN
//! (run_preset,'*')` are evaluated; rows for the other preset stay inert.
//!
//! A `Document` run additionally bans the shell structurally: `dispatch`
//! rejects `execute_command` deterministically before the store, the ladder,
//! and the sticky-group path, so even an `allow` rule must not pass
//! (deny-floor).
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
//!    `Ask` as `Allow` — except for `execute_command`, whose `Ask` never
//!    collapses and always parks (NEX-SEC-001 shell deny-floor). Unknown
//!    tools never reach the store (dispatch rejects them).

use rusqlite::params;
use serde::Serialize;

use crate::application::agent::approval::RiskClass;
use crate::infrastructure::database::{Database, DatabaseError};

/// Run preset (T5): which tool surface a run may use.
///
/// `Coding` (the default) exposes all six native tools. `Document` exposes
/// all except `execute_command`: the schema filter
/// ([`crate::application::agent::tools::ToolRegistry::definitions_for_preset`])
/// hides the shell from the model, and the dispatch layer denies it
/// deterministically (deny-floor: even an `allow` rule must not pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RunPreset {
    /// Full tool surface (default everywhere unless explicitly set).
    #[default]
    Coding,
    /// No shell: schema hides `execute_command`, dispatch denies it.
    Document,
}

impl RunPreset {
    /// Column/`group_key` value (`"coding"` / `"document"`).
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Document => "document",
        }
    }
}

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
///
/// Both sides are collapsed through [`normalize_rel`] first (NEX-SEC-002):
/// without it a `deny` rule on `private` misses `./private/x`, and the call
/// falls through to the ladder and auto-executes.
fn path_matches(rule_path: Option<&str>, request_path: Option<&str>) -> bool {
    match rule_path {
        None | Some("*") => true,
        Some(pattern) => match request_path {
            None => false,
            Some(request) => {
                let pattern = normalize_rel(pattern);
                let request = normalize_rel(request);
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

/// Collapse a workspace-relative path to canonical form for permission
/// matching (NEX-SEC-002).
///
/// Normalizes separators (`\` -> `/`), trims whitespace per segment,
/// collapses `.` and duplicate separators, and resolves `..` against the
/// workspace root (clamped: leading `..` cannot escape above the root).
/// Empty/root results map to `"*"`, matching [`normalize_dir`]'s root
/// convention so both helpers compose.
fn normalize_rel(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for component in slashed.split('/') {
        let component = component.trim();
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            parts.pop();
        } else {
            parts.push(component);
        }
    }
    if parts.is_empty() {
        "*".to_string()
    } else {
        parts.join("/")
    }
}

/// Extract the request path from a tool call's raw JSON arguments.
///
/// File tools (`read_file`, `write_file`, `edit_file`) normalise to the
/// parent directory; directory tools (`list_directory` `path`,
/// `execute_command` `cwd`) use the directory itself; `search_files` uses its
/// scope (`directory`, with `path` as alias — mirroring the executor's
/// fallback). Every arm is collapsed through [`normalize_rel`] so the
/// matcher and the executor (which canonicalizes) agree (NEX-SEC-002).
/// Returns `None` for pathless calls (or unparseable args); a
/// `None` request only matches pathless (`NULL`/`'*'`) rules.
#[must_use]
pub(crate) fn extract_path(tool_name: &str, arguments_json: &str) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    match tool_name {
        "read_file" | "write_file" | "edit_file" => {
            let path = args.get("path")?.as_str()?;
            if path.trim().is_empty() {
                return None;
            }
            Some(normalize_rel(&parent_dir(path)))
        }
        "list_directory" => {
            let path = args.get("path")?.as_str()?;
            if path.trim().is_empty() {
                return None;
            }
            Some(normalize_rel(&normalize_dir(path)))
        }
        "search_files" => {
            let scope = args
                .get("directory")
                .and_then(serde_json::Value::as_str)
                .or_else(|| args.get("path").and_then(serde_json::Value::as_str))?;
            if scope.trim().is_empty() {
                return None;
            }
            Some(normalize_rel(&normalize_dir(scope)))
        }
        "execute_command" => {
            let cwd = args.get("cwd")?.as_str()?;
            if cwd.trim().is_empty() {
                return None;
            }
            Some(normalize_rel(&normalize_dir(cwd)))
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

/// Whether a tool name is one of the six native tools (unknown tools never
/// reach the store).
#[must_use]
pub(crate) fn is_known_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_directory"
            | "write_file"
            | "execute_command"
            | "edit_file"
            | "search_files"
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

    #[test]
    fn known_tool_gate_covers_all_six_tools() {
        // H-2: `edit_file`/`search_files` used to bypass the store here.
        for tool in [
            "read_file",
            "list_directory",
            "write_file",
            "execute_command",
            "edit_file",
            "search_files",
        ] {
            assert!(is_known_tool(tool), "{tool} must reach the store");
        }
        assert!(!is_known_tool("unknown_tool"));
        assert!(!is_known_tool(""));
    }

    #[test]
    fn extract_path_covers_edit_file_and_search_files() {
        // edit_file behaves like the other file tools: parent directory.
        assert_eq!(
            extract_path(
                "edit_file",
                r#"{"path": "src/note.txt", "old_text": "a", "new_text": "b"}"#
            )
            .as_deref(),
            Some("src")
        );
        assert_eq!(
            extract_path(
                "edit_file",
                r#"{"path": "note.txt", "old_text": "a", "new_text": "b"}"#
            )
            .as_deref(),
            Some("*")
        );
        assert_eq!(
            extract_path("edit_file", r#"{"old_text": "a", "new_text": "b"}"#),
            None
        );
        assert_eq!(
            extract_path(
                "edit_file",
                r#"{"path": "  ", "old_text": "a", "new_text": "b"}"#
            ),
            None
        );
        // search_files uses its scope: `directory`, with `path` as alias
        // (mirroring the executor's fallback); missing/empty means root.
        assert_eq!(
            extract_path("search_files", r#"{"pattern": "x", "directory": "src"}"#).as_deref(),
            Some("src")
        );
        assert_eq!(
            extract_path("search_files", r#"{"pattern": "x", "path": "docs"}"#).as_deref(),
            Some("docs")
        );
        assert_eq!(
            extract_path(
                "search_files",
                r#"{"pattern": "x", "directory": "src", "path": "docs"}"#
            )
            .as_deref(),
            Some("src"),
            "directory wins over the path alias"
        );
        assert_eq!(extract_path("search_files", r#"{"pattern": "x"}"#), None);
        assert_eq!(
            extract_path("search_files", r#"{"pattern": "x", "directory": ""}"#),
            None
        );
        assert_eq!(extract_path("search_files", "not json"), None);
        // Untouched arms keep their behavior.
        assert_eq!(
            extract_path("write_file", r#"{"path": "a/b.txt", "content": "x"}"#).as_deref(),
            Some("a")
        );
        assert_eq!(extract_path("unknown_tool", r#"{"path": "a"}"#), None);
    }

    #[test]
    fn path_scoped_rules_hit_extracted_edit_and_search_paths() {
        // A rule on `private` matches the extracted parent of an edit target
        // under it, and the extracted scope of a search within it — so
        // path-scoped rules and group keys work for the two newly covered
        // tools exactly like for the original four.
        let store = PermissionStore::from_rules(vec![
            rule(
                1,
                "coding",
                "edit_file",
                Some("private"),
                RuleEffect::Deny,
                0,
            ),
            rule(
                2,
                "coding",
                "search_files",
                Some("private"),
                RuleEffect::Deny,
                0,
            ),
        ]);
        let edit_path = extract_path(
            "edit_file",
            r#"{"path": "private/note.txt", "old_text": "a", "new_text": "b"}"#,
        );
        assert_eq!(edit_path.as_deref(), Some("private"));
        assert_eq!(
            group_key("coding", "edit_file", edit_path.as_deref()),
            "coding:edit_file:private"
        );
        assert!(store
            .decide(
                "coding",
                "edit_file",
                edit_path.as_deref(),
                RiskClass::Mutating
            )
            .is_some());
        let search_path = extract_path(
            "search_files",
            r#"{"pattern": "x", "directory": "private/sub"}"#,
        );
        assert_eq!(search_path.as_deref(), Some("private/sub"));
        assert_eq!(
            group_key("coding", "search_files", search_path.as_deref()),
            "coding:search_files:private/sub"
        );
        assert!(store
            .decide(
                "coding",
                "search_files",
                search_path.as_deref(),
                RiskClass::ReadOnly
            )
            .is_some());
        // Outside the scope nothing matches (falls back to the ladder).
        assert!(store
            .decide("coding", "edit_file", Some("public"), RiskClass::Mutating)
            .is_none());
        assert!(store
            .decide(
                "coding",
                "search_files",
                Some("public"),
                RiskClass::ReadOnly
            )
            .is_none());
    }

    #[test]
    fn normalize_rel_table() {
        // NEX-SEC-002: every spelling the executor would resolve into the
        // workspace must collapse to one canonical form for matching.
        let cases = [
            ("private", "private"),
            ("./private", "private"),
            ("private/./sub", "private/sub"),
            ("private//sub", "private/sub"),
            ("//private//x//", "private/x"),
            ("a/../private", "private"),
            ("../private", "private"),
            ("..", "*"),
            (".", "*"),
            ("", "*"),
            ("./", "*"),
            ("private/", "private"),
            ("private ", "private"),
            (" private/sub ", "private/sub"),
            ("private\\sub", "private/sub"),
            (".\\private\\x", "private/x"),
            ("*", "*"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_rel(input), expected, "input={input:?}");
        }
    }

    #[test]
    fn deny_rule_on_private_blocks_dot_slash_evasion() {
        // NEX-SEC-002 regression: the executor resolves `./private/x` into
        // `private/`, so a deny rule on `private` must hit every spelling —
        // otherwise the call falls through to the ladder and auto-executes.
        let store = PermissionStore::from_rules(vec![rule(
            1,
            "coding",
            "write_file",
            Some("private"),
            RuleEffect::Deny,
            0,
        )]);
        for raw in [
            "private/secret.txt",
            "./private/secret.txt",
            ".\\private\\secret.txt",
            "private//secret.txt",
            "sub/../private/secret.txt",
            " ./private/secret.txt ",
            "./private/./secret.txt",
        ] {
            let args = format!(r#"{{"path": {raw:?}, "content": "x"}}"#);
            let path = extract_path("write_file", &args);
            let outcome =
                store.decide("coding", "write_file", path.as_deref(), RiskClass::Mutating);
            assert!(
                matches!(outcome, Some(PermissionOutcome::Deny { .. })),
                "raw={raw:?} extracted={path:?} must hit the deny rule"
            );
        }
        // Outside the scope nothing matches (falls back to the ladder).
        let outside = extract_path(
            "write_file",
            r#"{"path": "public/note.txt", "content": "x"}"#,
        );
        assert!(store
            .decide(
                "coding",
                "write_file",
                outside.as_deref(),
                RiskClass::Mutating
            )
            .is_none());
        // `..` that escapes the scope must not match either.
        assert!(store
            .decide("coding", "write_file", Some("other"), RiskClass::Mutating)
            .is_none());
    }

    #[test]
    fn rule_patterns_are_normalized_before_matching() {
        // A rule stored with an un-normalized pattern (`./private/`) still
        // scopes correctly once both sides collapse.
        let store = PermissionStore::from_rules(vec![rule(
            1,
            "coding",
            "read_file",
            Some("./private/"),
            RuleEffect::Deny,
            0,
        )]);
        assert!(store
            .decide(
                "coding",
                "read_file",
                Some("private/x"),
                RiskClass::ReadOnly
            )
            .is_some());
        assert!(store
            .decide("coding", "read_file", Some("public/x"), RiskClass::ReadOnly)
            .is_none());
    }
}
