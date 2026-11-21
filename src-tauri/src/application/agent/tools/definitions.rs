//! Native tool definitions for the agent workspace tools.
//! JSON-Schema descriptions of the six tools; execution lives in sibling modules.

use super::ToolRegistry;
use crate::application::agent::permissions::RunPreset;
use crate::application::agent::roles::TaskCapability;
use crate::application::execution::ToolDefinition;

impl ToolRegistry {
    /// Return JSON-Schema [`ToolDefinition`]s for the six native tools.
    ///
    /// Coding alias: the default preset exposes the full surface, so this
    /// stays byte-for-byte the pre-T5 list.
    pub(crate) fn definitions() -> Vec<ToolDefinition> {
        vec![
            execute_command_definition(),
            read_file_definition(),
            write_file_definition(),
            list_directory_definition(),
            edit_file_definition(),
            search_files_definition(),
            ask_user_question_definition(),
        ]
    }

    /// Return the tool schema for `preset` (T5): `Coding` exposes all six
    /// tools; `Document` exposes all except `execute_command` (structural
    /// shell ban — dispatch denies the shell deterministically even if a
    /// rule or a smuggled call lets it through).
    pub(crate) fn definitions_for_preset(preset: RunPreset) -> Vec<ToolDefinition> {
        match preset {
            RunPreset::Coding => Self::definitions(),
            RunPreset::Document => Self::definitions()
                .into_iter()
                .filter(|def| def.name != "execute_command")
                .collect(),
        }
    }

    /// Read-only tool subset permitted when a Plan run (`is_plan`) is active.
    pub(crate) fn definitions_for_read_only() -> Vec<ToolDefinition> {
        Self::definitions()
            .into_iter()
            .filter(|def| {
                def.name == "read_file"
                    || def.name == "list_directory"
                    || def.name == "search_files"
                    || def.name == "ask_user_question"
            })
            .collect()
    }

    /// Tool definitions exposed to the model for a specific run preset and plan mode.
    pub(crate) fn definitions_for_run(preset: RunPreset, is_plan: bool) -> Vec<ToolDefinition> {
        Self::definitions_for_run_with_mcp(preset, is_plan, &[])
    }

    /// Tool definitions exposed to the model for a specific run preset, plan
    /// mode, and discovered user-added MCP tools (`mcp__<server>__<tool>`).
    ///
    /// Plan mode (`is_plan`) is strictly read-only workspace access and NEVER
    /// exposes external MCP tools. Only valid namespaced MCP tool names that do
    /// not collide with native tools are appended.
    pub(crate) fn definitions_for_run_with_mcp(
        preset: RunPreset,
        is_plan: bool,
        mcp_tools: &[ToolDefinition],
    ) -> Vec<ToolDefinition> {
        Self::definitions_for_run_full(preset, is_plan, mcp_tools, false)
    }

    /// Tool definitions exposed to the model for a specific run preset, plan
    /// mode, discovered MCP tools, and parent-only subagent orchestration tools
    /// (`M19` / `P27`, `S11-07`).
    ///
    /// Subagents (`depth >= 1`) ALWAYS pass `include_subagents = false` so
    /// subagent tools are omitted structurally (max depth = 1).
    pub(crate) fn definitions_for_run_full(
        preset: RunPreset,
        is_plan: bool,
        mcp_tools: &[ToolDefinition],
        include_subagents: bool,
    ) -> Vec<ToolDefinition> {
        let mut defs = if is_plan {
            Self::definitions_for_read_only()
        } else {
            let mut base = Self::definitions_for_preset(preset);
            for mcp_def in mcp_tools {
                if crate::application::agent::permissions::is_mcp_tool(&mcp_def.name)
                    && !crate::application::agent::permissions::is_known_tool(&mcp_def.name)
                    && !crate::application::agent::permissions::is_subagent_tool(&mcp_def.name)
                    && !base.iter().any(|existing| existing.name == mcp_def.name)
                {
                    base.push(mcp_def.clone());
                }
            }
            base
        };
        if include_subagents {
            defs.extend(Self::subagent_definitions());
        }
        defs
    }

    /// Tool definitions exposed to the model for a run, further filtered by an
    /// optional [`TaskCapability`] ceiling (`P34`).
    ///
    /// Keeping `preset` unchanged while filtering via `capability` lets
    /// `ReadOnly` / `WorkspaceWrite` subagents hide disallowed tools (such as
    /// `execute_command`) without switching away from a `Coding` parent's
    /// preset-scoped permission rules.
    pub(crate) fn definitions_for_run_with_capability(
        preset: RunPreset,
        is_plan: bool,
        capability: Option<TaskCapability>,
        mcp_tools: &[ToolDefinition],
        include_subagents: bool,
    ) -> Vec<ToolDefinition> {
        let mut defs =
            Self::definitions_for_run_full(preset, is_plan, mcp_tools, include_subagents);
        if let Some(cap) = capability {
            defs.retain(|def| cap.allows_tool(&def.name));
        }
        defs
    }

    /// Parent-only subagent orchestration tools (`M19` / `P27`, `S11-07`).
    pub(crate) fn subagent_definitions() -> Vec<ToolDefinition> {
        vec![
            spawn_subagent_definition(),
            send_subagent_message_definition(),
            wait_subagent_definition(),
            kill_subagent_definition(),
            list_subagents_definition(),
        ]
    }
}

fn execute_command_definition() -> ToolDefinition {
    ToolDefinition {
        name: "execute_command".to_string(),
        description: "Run a shell command with a 30s timeout, capturing stdout and stderr. Executes inside the workspace. Output is truncated to 20KB.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute (e.g. \"echo hello\" or \"cargo test\")"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory relative to workspace root (optional). Must be inside workspace."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
    }
}

fn read_file_definition() -> ToolDefinition {
    ToolDefinition {
        name: "read_file".to_string(),
        description:
            "Read a file inside the workspace. Supports line offset and limit for large files."
                .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file relative to workspace root"
                },
                "offset_lines": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Line offset to start reading from (0-indexed)"
                },
                "limit_lines": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of lines to return"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    }
}

fn write_file_definition() -> ToolDefinition {
    ToolDefinition {
        name: "write_file".to_string(),
        description:
            "Write or overwrite a file inside the workspace, creating parent directories as needed."
                .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Destination path relative to workspace root"
                },
                "content": {
                    "type": "string",
                    "description": "Text content to write"
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
    }
}

fn list_directory_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_directory".to_string(),
        description: "List directory contents inside the workspace. Use recursive=true to walk subdirectories.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path relative to workspace root (defaults to workspace root)"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Whether to list recursively"
                }
            },
            "required": [],
            "additionalProperties": false
        }),
    }
}

fn edit_file_definition() -> ToolDefinition {
    ToolDefinition {
        name: "edit_file".to_string(),
        description: "Replace one exact occurrence of old_text with new_text, or insert new_text verbatim directly after the single occurrence of insert_after. Supply exactly one of old_text or insert_after. The anchor must occur exactly once: zero matches fail with 'no exact match found' and two or more matches fail rather than guessing. Confined to the workspace; the file must be valid UTF-8. Returns a unified diff of the change.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file relative to workspace root"
                },
                "old_text": {
                    "type": "string",
                    "description": "Exact text to replace (must occur exactly once in the file)"
                },
                "new_text": {
                    "type": "string",
                    "description": "Replacement text (replace mode) or text to insert verbatim directly after the anchor (insert mode); include any newlines"
                },
                "insert_after": {
                    "type": "string",
                    "description": "Anchor text; new_text is inserted verbatim directly after its single occurrence"
                }
            },
            "required": ["path", "new_text"],
            "additionalProperties": false
        }),
    }
}

fn search_files_definition() -> ToolDefinition {
    ToolDefinition {
        name: "search_files".to_string(),
        description: "Search file contents inside the workspace with a regex-lite pattern; one path:line:text hit per matching line. Supported syntax: literal characters, . (any single character), * + ? quantifiers on the preceding element, ^ start and $ end anchors, and classes \\d \\D \\w \\W \\s \\S. Groups (), alternation |, character classes [], and {n,m} counts are NOT supported and match literally. Matching is case-sensitive and bounded (patterns over 10KB rejected; huge single lines scanned under a step budget). Binary files, files over 5MB, unreadable files, and symbolic links are skipped; the walk never leaves the workspace. Long lines are middle-truncated with an edge-kept notice. `path` is an alias of `directory` (either scopes the search).".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex-lite pattern (see tool description for the supported subset)"
                },
                "directory": {
                    "type": "string",
                    "description": "Directory scope relative to workspace root (defaults to workspace root; `path` is an alias)"
                },
                "path": {
                    "type": "string",
                    "description": "Alias of `directory`: directory scope relative to workspace root"
                },
                "max_matches": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum matches to return (default 50, hard cap 200)"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    }
}

fn ask_user_question_definition() -> ToolDefinition {
    ToolDefinition {
        name: "ask_user_question".to_string(),
        description: "Ask the user a structured clarifying question and pause this run until they answer. Supports single-select options, multi-select options, and optional free-text input. Use when requirements are genuinely ambiguous and user input is needed to proceed accurately.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "Clear, concise question prompt to present to the user"
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of selectable choices for the user"
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "Whether the user may select more than one option (default false)"
                },
                "allow_free_text": {
                    "type": "boolean",
                    "description": "Whether the user may enter a free-text answer (default true)"
                }
            },
            "required": ["question"],
            "additionalProperties": false
        }),
    }
}

fn spawn_subagent_definition() -> ToolDefinition {
    ToolDefinition {
        name: "spawn_subagent".to_string(),
        description: "Spawn a visible task-scoped subagent to investigate or implement a focused subtask in parallel. Defaults to the parent's model (never auto-escalates) and strictly obeys child_access <= parent_current_access. Set use_worktree=true for isolated parallel writes in a Git repository; read-only subagents share the parent workspace without copying.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Clear, self-contained task prompt for the subagent"
                },
                "title": {
                    "type": "string",
                    "description": "Short label displayed on the subagent card (optional)"
                },
                "access": {
                    "type": "string",
                    "enum": ["read_only", "workspace_write", "full_access"],
                    "description": "Requested capability scope (default 'read_only'; strictly capped by parent's current access)"
                },
                "use_worktree": {
                    "type": "boolean",
                    "description": "Run a writing subagent inside an isolated temporary git worktree and apply its patch cleanly on completion (default false; ignored for read-only subagents)"
                },
                "model": {
                    "type": "string",
                    "description": "Optional explicit model override on the same provider (defaults to parent model)"
                },
                "effort": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "xhigh"],
                    "description": "Optional reasoning effort override validated against the model catalog's supported_efforts"
                },
                "max_turns": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 30,
                    "description": "Maximum LLM turns for this subagent (default 12)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 5,
                    "maximum": 900,
                    "description": "Wall-clock timeout in seconds (default 180)"
                },
                "wait": {
                    "type": "boolean",
                    "description": "Wait for the subagent to finish before returning (default false for parallel execution)"
                }
            },
            "required": ["task"],
            "additionalProperties": false
        }),
    }
}

fn send_subagent_message_definition() -> ToolDefinition {
    ToolDefinition {
        name: "send_subagent_message".to_string(),
        description: "Send a follow-up message to an existing completed/idle subagent, continuing inside its preserved conversation context.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "subagent_id": {
                    "type": "string",
                    "description": "Identifier of the existing subagent (e.g. 'sub-1')"
                },
                "message": {
                    "type": "string",
                    "description": "Follow-up instruction for the subagent"
                },
                "max_turns": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 30,
                    "description": "Optional turn limit for this continuation"
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 5,
                    "maximum": 900,
                    "description": "Optional wall-clock timeout in seconds"
                },
                "wait": {
                    "type": "boolean",
                    "description": "Wait for the follow-up turn to finish before returning (default true)"
                }
            },
            "required": ["subagent_id", "message"],
            "additionalProperties": false
        }),
    }
}

fn wait_subagent_definition() -> ToolDefinition {
    ToolDefinition {
        name: "wait_subagent".to_string(),
        description: "Wait for one specific subagent, a list of subagents, or all currently running subagents to finish and return their results.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "subagent_id": {
                    "type": "string",
                    "description": "Single subagent ID to wait for (optional)"
                },
                "subagent_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Multiple subagent IDs to wait for (optional; omit both to wait for all active subagents)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 900,
                    "description": "Maximum seconds to wait (default 300)"
                }
            },
            "required": [],
            "additionalProperties": false
        }),
    }
}

fn kill_subagent_definition() -> ToolDefinition {
    ToolDefinition {
        name: "kill_subagent".to_string(),
        description: "Stop one individual subagent and clean up its temporary worktree (if any) without cancelling the parent run.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "subagent_id": {
                    "type": "string",
                    "description": "Identifier of the subagent to stop"
                }
            },
            "required": ["subagent_id"],
            "additionalProperties": false
        }),
    }
}

fn list_subagents_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_subagents".to_string(),
        description: "List all subagents spawned in the current run along with their status, model, access level, worktree isolation, and step counts.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn definitions_produce_valid_objects() {
        let defs = ToolRegistry::definitions();
        assert_eq!(defs.len(), 7);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"execute_command"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"list_directory"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"search_files"));
        assert!(names.contains(&"ask_user_question"));
        for def in &defs {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
            assert!(def.parameters.is_object());
            let obj = def.parameters.as_object().unwrap();
            assert_eq!(obj.get("type").unwrap(), "object");
            assert!(obj.contains_key("properties"));
        }
        // Check specific schemas
        let exec = defs.iter().find(|d| d.name == "execute_command").unwrap();
        let props = exec.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("command"));
        assert!(props.contains_key("cwd"));
        let req = exec.parameters["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "command"));

        let read = defs.iter().find(|d| d.name == "read_file").unwrap();
        let rprops = read.parameters["properties"].as_object().unwrap();
        assert!(rprops.contains_key("path"));
        assert!(rprops.contains_key("offset_lines"));
        assert!(rprops.contains_key("limit_lines"));

        let write = defs.iter().find(|d| d.name == "write_file").unwrap();
        let wprops = write.parameters["properties"].as_object().unwrap();
        assert!(wprops.contains_key("path"));
        assert!(wprops.contains_key("content"));

        let list = defs.iter().find(|d| d.name == "list_directory").unwrap();
        let lprops = list.parameters["properties"].as_object().unwrap();
        assert!(lprops.contains_key("path"));
        assert!(lprops.contains_key("recursive"));

        let edit = defs.iter().find(|d| d.name == "edit_file").unwrap();
        let eprops = edit.parameters["properties"].as_object().unwrap();
        assert!(eprops.contains_key("path"));
        assert!(eprops.contains_key("old_text"));
        assert!(eprops.contains_key("new_text"));
        assert!(eprops.contains_key("insert_after"));

        let search = defs.iter().find(|d| d.name == "search_files").unwrap();
        let sprops = search.parameters["properties"].as_object().unwrap();
        assert!(sprops.contains_key("pattern"));
        assert!(sprops.contains_key("directory"));
        assert!(sprops.contains_key("max_matches"));
    }

    #[test]
    fn search_files_schema_documents_path_alias() {
        // `path` is a documented alias of `directory` (the executor already
        // falls back to it); the schema must advertise both.
        let defs = ToolRegistry::definitions();
        let search = defs.iter().find(|d| d.name == "search_files").unwrap();
        assert!(
            search
                .description
                .contains("`path` is an alias of `directory`"),
            "alias must be documented: {}",
            search.description
        );
        let sprops = search.parameters["properties"].as_object().unwrap();
        assert!(sprops.contains_key("path"));
        assert!(sprops.contains_key("directory"));
    }

    #[test]
    fn definitions_are_json_schema_valid() {
        for def in ToolRegistry::definitions() {
            // Parameters must be valid JSON schema object
            let v = &def.parameters;
            assert!(v.is_object());
            // Must contain type: object
            assert_eq!(v["type"], "object");
            // Unknown tool definitions should not have empty name
            assert!(!def.name.is_empty());
            // Round-trip through ToolDefinition serialization
            let json = serde_json::to_string(&def).unwrap();
            let back: ToolDefinition = serde_json::from_str(&json).unwrap();
            assert_eq!(def, back);
        }
    }

    #[test]
    fn preset_matrices_coding_has_shell_document_has_not() {
        // Coding exposes the full surface; document exposes all except the
        // shell; the other five tools are present in both.
        let coding = ToolRegistry::definitions_for_preset(RunPreset::Coding);
        let document = ToolRegistry::definitions_for_preset(RunPreset::Document);
        let coding_names: Vec<&str> = coding.iter().map(|d| d.name.as_str()).collect();
        let document_names: Vec<&str> = document.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(coding.len(), 7, "coding exposes all seven tools");
        assert!(
            coding_names.contains(&"execute_command"),
            "coding keeps the shell, got {coding_names:?}"
        );
        assert_eq!(document.len(), 6, "document drops exactly the shell");
        assert!(
            !document_names.contains(&"execute_command"),
            "document must not expose the shell, got {document_names:?}"
        );
        for tool in [
            "read_file",
            "write_file",
            "list_directory",
            "edit_file",
            "search_files",
            "ask_user_question",
        ] {
            assert!(
                coding_names.contains(&tool),
                "coding must expose {tool}, got {coding_names:?}"
            );
            assert!(
                document_names.contains(&tool),
                "document must expose {tool}, got {document_names:?}"
            );
        }
        // `definitions()` stays the coding alias (frozen coding behavior).
        assert_eq!(ToolRegistry::definitions(), coding);
    }

    // ---- Gemini wire-reduction pins (moved from the Gemini executor's test
    // module, NEX-ARCH-002: the infrastructure tests may no longer import the
    // application `ToolRegistry`, so the sync pins live beside the
    // definitions they pin and drive the Gemini body builder through the
    // crate-visible `generate_content_request`).

    use crate::infrastructure::providers::gemini::{
        generate_content_request, PROVIDER_NAME as GEMINI_PROVIDER_NAME,
    };
    use nexora_core::provider::{AiMessage, AiRequest, AiRole};

    /// True when `key` appears anywhere inside `value` (objects or arrays).
    fn contains_key_recursive(value: &serde_json::Value, key: &str) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.contains_key(key)
                    || map.values().any(|inner| contains_key_recursive(inner, key))
            }
            serde_json::Value::Array(items) => {
                items.iter().any(|inner| contains_key_recursive(inner, key))
            }
            _ => false,
        }
    }

    /// An [`AiRequest`] carrying the four real workspace tool definitions.
    fn tool_request(tools: Vec<ToolDefinition>) -> AiRequest {
        AiRequest {
            provider: GEMINI_PROVIDER_NAME.to_string(),
            model: "gemini-3.6-flash".to_string(),
            messages: std::sync::Arc::new(vec![AiMessage {
                role: AiRole::User,
                content: "Run the tests".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }]),
            tools: std::sync::Arc::new(tools),
            model_config: None,
            request_timeout: None,
        }
    }

    /// Test (a): the six real tool definitions, serialized through
    /// `generate_content_request`, contain none of the JSON-Schema keys Gemini
    /// rejects; `read_file`'s parameters equal the exact reduced shape; the
    /// empty `required` of `list_directory` is dropped entirely.
    #[test]
    fn agent_tool_declarations_are_reduced_to_the_gemini_subset() {
        let request = tool_request(ToolRegistry::definitions());
        let body = generate_content_request(&request);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("serialize")).expect("parse");
        let declarations = value["tools"][0]["functionDeclarations"]
            .as_array()
            .expect("seven function declarations");
        assert_eq!(declarations.len(), 7, "all seven tools are declared");

        for declaration in declarations {
            let parameters = &declaration["parameters"];
            for forbidden in [
                "additionalProperties",
                "minimum",
                "maximum",
                "const",
                "propertyNames",
                "$schema",
                "$defs",
            ] {
                assert!(
                    !contains_key_recursive(parameters, forbidden),
                    "declaration '{}' leaked forbidden key '{forbidden}': {parameters}",
                    declaration["name"]
                );
            }
        }

        // read_file: type / description / properties / non-empty required
        // survive unchanged; minimum and additionalProperties are gone.
        let read_file = &declarations[1];
        assert_eq!(read_file["name"], "read_file");
        let expected = serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file relative to workspace root"
                },
                "offset_lines": {
                    "type": "integer",
                    "description": "Line offset to start reading from (0-indexed)"
                },
                "limit_lines": {
                    "type": "integer",
                    "description": "Maximum number of lines to return"
                }
            },
            "required": ["path"]
        });
        assert_eq!(
            read_file["parameters"], expected,
            "read_file parameters must equal the exact reduced shape"
        );

        // list_directory: `required: []` is dropped entirely (tools.rs:175).
        let list_directory = &declarations[3];
        assert_eq!(list_directory["name"], "list_directory");
        assert!(
            list_directory["parameters"].get("required").is_none(),
            "list_directory must not carry a 'required' key"
        );

        // Non-empty required arrays of the other tools survive verbatim.
        assert_eq!(
            declarations[0]["parameters"]["required"],
            serde_json::json!(["command"]),
            "execute_command keeps its required array"
        );
        assert_eq!(
            declarations[2]["parameters"]["required"],
            serde_json::json!(["path", "content"]),
            "write_file keeps its required array"
        );
        assert_eq!(
            declarations[4]["parameters"]["required"],
            serde_json::json!(["path", "new_text"]),
            "edit_file keeps its required array"
        );
        assert_eq!(declarations[4]["name"], "edit_file");
        assert_eq!(
            declarations[5]["parameters"]["required"],
            serde_json::json!(["pattern"]),
            "search_files keeps its required array"
        );
        assert_eq!(declarations[5]["name"], "search_files");
    }

    /// Test (b): the reduction is a Gemini wire adapter only. The shared
    /// `AiRequest.tools` payload — the exact value the OpenAI-compatible
    /// (`chat_completion_request`, `openai.rs:190`) and Anthropic-native
    /// (`anthropic_request`, `anthropic.rs:235`) body builders serialize
    /// verbatim into their `tools` arrays — keeps the full JSON Schema, and
    /// `generate_content_request` never mutates it.
    #[test]
    fn other_providers_keep_the_schema_verbatim() {
        let definitions = ToolRegistry::definitions();
        let shared = definitions.clone();

        // The shared source of truth still carries the keys Gemini rejects;
        // OpenAI and Anthropic serialize exactly this value.
        for definition in &shared {
            assert!(
                definition.parameters.get("additionalProperties").is_some(),
                "shared definition '{}' lost additionalProperties",
                definition.name
            );
        }
        let read_file = &shared[1];
        assert_eq!(read_file.name, "read_file");
        assert!(
            contains_key_recursive(&read_file.parameters, "minimum"),
            "shared read_file schema lost its 'minimum' constraints"
        );

        // Driving the definitions through the Gemini body builder leaves the
        // shared value untouched, and the reduction appears only in the
        // Gemini wire body (a passthrough would leak the keys below).
        let request = tool_request(definitions);
        let body = generate_content_request(&request);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("serialize")).expect("parse");
        let declarations = value["tools"][0]["functionDeclarations"]
            .as_array()
            .expect("declarations");
        for declaration in declarations {
            assert!(
                !contains_key_recursive(&declaration["parameters"], "additionalProperties"),
                "gemini body for '{}' still carries additionalProperties: {}",
                declaration["name"],
                declaration["parameters"]
            );
        }
        for definition in &shared {
            assert!(
                definition.parameters.get("additionalProperties").is_some(),
                "shared definition '{}' was mutated by the gemini builder",
                definition.name
            );
        }
    }
}
