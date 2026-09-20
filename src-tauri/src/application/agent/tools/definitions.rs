//! Native tool definitions for the agent workspace tools.
//! JSON-Schema descriptions of the four tools; execution lives in sibling modules.

use super::ToolRegistry;
use crate::application::execution::ToolDefinition;

impl ToolRegistry {
    /// Return JSON-Schema [`ToolDefinition`]s for the four native tools.
    pub(crate) fn definitions() -> Vec<ToolDefinition> {
        vec![
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
            },
            ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file inside the workspace. Supports line offset and limit for large files.".to_string(),
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
            },
            ToolDefinition {
                name: "write_file".to_string(),
                description: "Write or overwrite a file inside the workspace, creating parent directories as needed.".to_string(),
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
            },
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
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn definitions_produce_valid_objects() {
        let defs = ToolRegistry::definitions();
        assert_eq!(defs.len(), 4);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"execute_command"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"list_directory"));
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
}
