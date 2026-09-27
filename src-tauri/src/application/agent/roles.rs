//! Typed agent roles with role-scoped tool subsets (WS-B.3).
//!
//! Six hardcoded roles (pure data, like `SUPPORTED_MODELS`): each role pins
//! an allowed-tool subset of the six native tools, a default [`TaskKey`], and
//! an approval-posture reference. The posture only *names* the #65 outcome
//! ([`RiskClass`], reused via [`AgentRole::risk_class`]) — it never remaps
//! the ladder, the permission store, or the `FullAutonomous` policy.
//!
//! Enforcement composes with the existing dispatch path: [`DispatchCtx`]
//! carries an optional role, and a known tool outside the role's subset
//! becomes a controlled denial observation before the store, the ladder, and
//! execution (mirroring the T5 document shell ban). Runs without an attached
//! role keep the exact pre-B.3 behavior.
//!
//! [`DispatchCtx`]: super::dispatch::DispatchCtx

use serde::{Deserialize, Serialize};

use super::approval::RiskClass;
use super::lifecycle::TaskKey;

// ---------------------------------------------------------------------------
// Role
// ---------------------------------------------------------------------------

/// Typed agent role (WS-B.3): pure data, hardcoded like `SUPPORTED_MODELS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentRole {
    /// Plans work from read-only observation; never mutates.
    Planner,
    /// Gathers context from read-only observation; never mutates.
    Researcher,
    /// Mutates files but never runs the shell.
    Implementer,
    /// Reviews work from read-only observation; never mutates.
    Reviewer,
    /// Full six-tool surface; the default for `agent` runs.
    Executor,
    /// Minimal read pair; the default for `chat` runs.
    Observer,
}

/// Every role, exactly once (pinned for the matrix tests).
pub(crate) const ALL_ROLES: [AgentRole; 6] = [
    AgentRole::Planner,
    AgentRole::Researcher,
    AgentRole::Implementer,
    AgentRole::Reviewer,
    AgentRole::Executor,
    AgentRole::Observer,
];

impl AgentRole {
    /// Canonical role name (`planner` / ... / `observer`).
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Researcher => "researcher",
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
            Self::Executor => "executor",
            Self::Observer => "observer",
        }
    }

    /// Parse a role name. Unknown names fail loudly with the secret-free
    /// [`RoleError`] — there is intentionally no silent default.
    ///
    /// # Errors
    ///
    /// Returns [`RoleError`] when `name` is not one of the six canonical
    /// role names.
    pub(crate) fn parse(name: &str) -> Result<Self, RoleError> {
        match name {
            "planner" => Ok(Self::Planner),
            "researcher" => Ok(Self::Researcher),
            "implementer" => Ok(Self::Implementer),
            "reviewer" => Ok(Self::Reviewer),
            "executor" => Ok(Self::Executor),
            "observer" => Ok(Self::Observer),
            _ => Err(RoleError),
        }
    }

    /// Allowed-tool subset of the six native tools, in canonical tool order.
    ///
    /// Read-only roles share the read-only trio; the implementer adds file
    /// mutation (no shell); the executor exposes the full surface; the
    /// observer keeps the minimal read pair (no search, no mutation, no
    /// shell).
    #[must_use]
    pub(crate) const fn allowed_tools(self) -> &'static [&'static str] {
        match self {
            Self::Planner | Self::Researcher | Self::Reviewer => {
                &["read_file", "list_directory", "search_files"]
            }
            Self::Implementer => &[
                "read_file",
                "list_directory",
                "write_file",
                "edit_file",
                "search_files",
            ],
            Self::Executor => &[
                "read_file",
                "list_directory",
                "write_file",
                "execute_command",
                "edit_file",
                "search_files",
            ],
            Self::Observer => &["read_file", "list_directory"],
        }
    }

    /// Whether `tool` is inside this role's subset.
    #[must_use]
    pub(crate) fn allows_tool(self, tool: &str) -> bool {
        self.allowed_tools().contains(&tool)
    }

    /// Default task key served by this role.
    #[must_use]
    pub(crate) const fn default_task_key(self) -> TaskKey {
        match self {
            Self::Observer => TaskKey::Chat,
            Self::Planner
            | Self::Researcher
            | Self::Implementer
            | Self::Reviewer
            | Self::Executor => TaskKey::Agent,
        }
    }

    /// Default role for a task key: `executor` for `agent` runs, `observer`
    /// for `chat` runs.
    #[must_use]
    pub(crate) const fn default_for_task(task: TaskKey) -> Self {
        match task {
            TaskKey::Agent => Self::Executor,
            TaskKey::Chat => Self::Observer,
        }
    }

    /// Approval-posture reference: names the #65 [`RiskClass`] outcome that
    /// governs this role's tools. The ladder itself is reused unchanged —
    /// never bypassed, never remapped.
    #[must_use]
    pub(crate) const fn approval_posture(self) -> &'static str {
        match self {
            Self::Planner | Self::Researcher | Self::Reviewer => {
                "read-only subset: RiskClass::ReadOnly tools follow the gate ladder, \
                 RiskClass::Mutating tools are unreachable; #65 ladder reused unchanged"
            }
            Self::Implementer => {
                "file-mutation subset: RiskClass::Mutating file tools follow the gate ladder, \
                 the shell is unreachable; #65 ladder reused unchanged"
            }
            Self::Executor => {
                "full surface: every tool follows its RiskClass gate outcome; \
                 #65 ladder reused unchanged"
            }
            Self::Observer => {
                "minimal read subset: RiskClass::ReadOnly tools follow the gate ladder, \
                 all else unreachable; #65 ladder reused unchanged"
            }
        }
    }

    /// The #65 [`RiskClass`] outcome for `tool`, reused unchanged (never
    /// remapped per role: every role sees the same classification).
    #[must_use]
    pub(crate) fn risk_class(tool_name: &str) -> RiskClass {
        RiskClass::classify(tool_name)
    }
}

/// Resolve the run role: an explicit name wins, otherwise the task-key
/// default applies. Unknown names fail loudly — never a silent default.
///
/// # Errors
///
/// Returns the secret-free [`RoleError`] when `name` carries an unknown role.
pub(crate) fn resolve_role(task: TaskKey, name: Option<&str>) -> Result<AgentRole, RoleError> {
    match name {
        None => Ok(AgentRole::default_for_task(task)),
        Some(explicit) => AgentRole::parse(explicit),
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Secret-free unknown-role error: fixed vocabulary only, never echoes the
/// rejected input (which may carry hostile content).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RoleError;

impl std::fmt::Display for RoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown agent role")
    }
}

impl std::error::Error for RoleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::permissions::is_known_tool;
    use crate::application::agent::runner::test_support::*;
    use crate::application::agent::runner::AgentRunner;
    use std::fs;

    const SIX_TOOLS: [&str; 6] = [
        "read_file",
        "list_directory",
        "write_file",
        "execute_command",
        "edit_file",
        "search_files",
    ];

    #[test]
    fn all_six_roles_parse_and_round_trip() {
        assert_eq!(ALL_ROLES.len(), 6);
        for (index, role) in ALL_ROLES.iter().enumerate() {
            for other in &ALL_ROLES[index + 1..] {
                assert_ne!(role, other, "roles must be distinct");
            }
            let name = role.as_str();
            assert_eq!(
                AgentRole::parse(name).expect("canonical name parses"),
                *role
            );
            let raw = serde_json::to_string(role).expect("serialize role");
            assert!(raw.contains(name), "role serializes snake_case, got {raw}");
            let back: AgentRole = serde_json::from_str(&raw).expect("round-trip role");
            assert_eq!(&back, role);
        }
    }

    #[test]
    fn unknown_role_errors_loudly_without_echo() {
        for hostile in [
            "admin",
            "",
            "EXECUTOR",
            "executor ",
            "sk-live-secret SELECT * FROM users",
            "../../etc/passwd",
        ] {
            let err = AgentRole::parse(hostile).expect_err("unknown role must fail");
            assert_eq!(format!("{err}"), "unknown agent role");
        }
        // Resolution never falls back silently either.
        assert!(resolve_role(TaskKey::Agent, Some("planner-ish")).is_err());
        assert!(resolve_role(TaskKey::Chat, Some("")).is_err());
    }

    #[test]
    fn tool_matrix_pins_every_cell() {
        // Expected subset per role, in canonical tool order.
        let expected: [(AgentRole, &[&str]); 6] = [
            (
                AgentRole::Planner,
                &["read_file", "list_directory", "search_files"],
            ),
            (
                AgentRole::Researcher,
                &["read_file", "list_directory", "search_files"],
            ),
            (
                AgentRole::Implementer,
                &[
                    "read_file",
                    "list_directory",
                    "write_file",
                    "edit_file",
                    "search_files",
                ],
            ),
            (
                AgentRole::Reviewer,
                &["read_file", "list_directory", "search_files"],
            ),
            (AgentRole::Executor, &SIX_TOOLS),
            (AgentRole::Observer, &["read_file", "list_directory"]),
        ];
        assert_eq!(expected.len(), ALL_ROLES.len());
        for (role, tools) in expected {
            assert_eq!(role.allowed_tools(), tools, "{role:?} subset pinned");
            for tool in SIX_TOOLS {
                assert!(is_known_tool(tool), "{tool} must be a known tool");
                assert_eq!(
                    role.allows_tool(tool),
                    tools.contains(&tool),
                    "{role:?} x {tool} pinned"
                );
            }
        }
        assert_eq!(AgentRole::Planner.allowed_tools().len(), 3);
        assert_eq!(AgentRole::Researcher.allowed_tools().len(), 3);
        assert_eq!(AgentRole::Implementer.allowed_tools().len(), 5);
        assert_eq!(AgentRole::Reviewer.allowed_tools().len(), 3);
        assert_eq!(AgentRole::Executor.allowed_tools().len(), 6);
        assert_eq!(AgentRole::Observer.allowed_tools().len(), 2);
    }

    #[test]
    fn defaults_follow_task_key() {
        assert_eq!(
            AgentRole::default_for_task(TaskKey::Agent),
            AgentRole::Executor
        );
        assert_eq!(
            AgentRole::default_for_task(TaskKey::Chat),
            AgentRole::Observer
        );
        assert_eq!(
            resolve_role(TaskKey::Agent, None).expect("agent default"),
            AgentRole::Executor
        );
        assert_eq!(
            resolve_role(TaskKey::Chat, None).expect("chat default"),
            AgentRole::Observer
        );
        assert_eq!(
            resolve_role(TaskKey::Chat, Some("executor")).expect("explicit wins"),
            AgentRole::Executor
        );
        assert_eq!(AgentRole::Observer.default_task_key(), TaskKey::Chat);
        for role in [
            AgentRole::Planner,
            AgentRole::Researcher,
            AgentRole::Implementer,
            AgentRole::Reviewer,
            AgentRole::Executor,
        ] {
            assert_eq!(role.default_task_key(), TaskKey::Agent);
        }
    }

    #[test]
    fn approval_posture_names_risk_class_without_remap() {
        for role in ALL_ROLES {
            let posture = role.approval_posture();
            assert!(
                posture.contains("RiskClass"),
                "{role:?} posture must name the RiskClass outcome, got {posture:?}"
            );
            assert!(
                posture.contains("reused unchanged"),
                "{role:?} posture must pin reuse, got {posture:?}"
            );
            assert!(
                !posture.to_lowercase().contains("remap"),
                "{role:?} posture must never claim a remap, got {posture:?}"
            );
        }
        // Delegation, not remapping: identical to the #65 classification for
        // every native tool plus unknown tools.
        for tool in SIX_TOOLS.into_iter().chain(["does_not_exist", ""]) {
            assert_eq!(
                AgentRole::risk_class(tool),
                RiskClass::classify(tool),
                "risk outcome for {tool:?} must match #65"
            );
        }
    }

    #[test]
    fn observer_role_denies_out_of_subset_tool() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(crate::application::execution::AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![call_tool(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "blocked.txt", "content": "x"}),
                )],
                usage: None,
            }),
            Ok(text_response("recovered")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_role(AgentRole::Observer);

        let answer = runner
            .run("openai", "m", "cred", "q")
            .expect("denial continues");
        assert_eq!(answer, "recovered");
        assert!(
            fs::read_to_string(ws.join("blocked.txt")).is_err(),
            "out-of-subset tool must not execute"
        );

        let history = &fake.requests.borrow()[1].messages;
        let result = history[3]
            .tool_result
            .as_ref()
            .expect("tool result present");
        assert_eq!(
            result.content,
            "Error: tool 'write_file' is not available to agent role 'observer'"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn in_subset_tools_execute_identically_under_role() {
        let ws = temp_workspace();
        fs::write(ws.join("note.txt"), "kept").expect("seed");
        let fake = FakeExecutor::new(vec![
            Ok(crate::application::execution::AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![call_tool(
                    "r1",
                    "read_file",
                    serde_json::json!({"path": "note.txt"}),
                )],
                usage: None,
            }),
            Ok(text_response("saw it")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_role(AgentRole::Observer);

        let answer = runner
            .run("openai", "m", "cred", "q")
            .expect("role read works");
        assert_eq!(answer, "saw it");
        let history = &fake.requests.borrow()[1].messages;
        let result = history[3]
            .tool_result
            .as_ref()
            .expect("tool result present");
        assert_eq!(result.content, "kept");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn unknown_tools_keep_their_path_under_role() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(crate::application::execution::AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![raw_call("u1", "does_not_exist", "{}")],
                usage: None,
            }),
            Ok(text_response("recovered")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_role(AgentRole::Observer);

        let answer = runner
            .run("openai", "m", "cred", "q")
            .expect("unknown continues");
        assert_eq!(answer, "recovered");
        let history = &fake.requests.borrow()[1].messages;
        let result = history[3]
            .tool_result
            .as_ref()
            .expect("tool result present");
        assert!(
            result.content.contains("unknown tool"),
            "role gate must not shadow the unknown-tool path: {}",
            result.content
        );
        let _ = fs::remove_dir_all(&ws);
    }
}
