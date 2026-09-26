//! Native tool definitions for the agent workspace tools.
//! JSON-Schema descriptions of the six tools; execution lives in sibling modules.

use super::ToolRegistry;
use crate::application::agent::permissions::RunPreset;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn definitions_produce_valid_objects() {
        let defs = ToolRegistry::definitions();
        assert_eq!(defs.len(), 6);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"execute_command"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"list_directory"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"search_files"));
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
        assert_eq!(coding.len(), 6, "coding exposes all six tools");
        assert!(
            coding_names.contains(&"execute_command"),
            "coding keeps the shell, got {coding_names:?}"
        );
        assert_eq!(document.len(), 5, "document drops exactly the shell");
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
}
