//! JSON-RPC 2.0 wire types, MCP protocol payloads, bounded response decoding,
//! and strict `mcp__<server>__<tool>` tool-name namespacing.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::application::agent::injection::redact_secrets;
use crate::application::execution::ToolDefinition;

/// MCP protocol version advertised during `initialize`.
pub(crate) const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

/// Required prefix on every MCP tool exposed to the model and dispatched by
/// the agent loop (`mcp__<server>__<tool>`).
pub(crate) const MCP_TOOL_PREFIX: &str = "mcp__";

/// Maximum length of a namespaced MCP tool name (`mcp__<server>__<tool>`).
pub(crate) const MAX_NAMESPACED_TOOL_LEN: usize = 128;

/// Maximum length of a tool description exposed to the model.
pub(crate) const MAX_TOOL_DESCRIPTION_LEN: usize = 512;

/// Maximum tools accepted from a single MCP server during `tools/list`.
pub(crate) const MAX_TOOLS_PER_SERVER: usize = 128;

/// Maximum pagination hops for `tools/list` (`nextCursor`).
pub(crate) const MAX_TOOLS_LIST_PAGES: usize = 16;

/// Hard cap on a single JSON-RPC line or HTTP response body (2 MiB).
pub(crate) const MAX_JSONRPC_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;

/// Hard cap on formatted text output returned from a single `tools/call` (64 KiB).
pub(crate) const MAX_MCP_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct JsonRpcRequest<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl<'a> JsonRpcRequest<'a> {
    #[must_use]
    pub(crate) fn new(id: u64, method: &'a str, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method,
            params,
        }
    }
}

/// JSON-RPC 2.0 notification envelope (no `id`).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct JsonRpcNotification<'a> {
    pub jsonrpc: &'static str,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl<'a> JsonRpcNotification<'a> {
    #[must_use]
    pub(crate) fn new(method: &'a str, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            method,
            params,
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsonRpcErrorObj {
    pub code: i64,
    #[serde(default)]
    pub message: String,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsonRpcResponse {
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcErrorObj>,
}

impl JsonRpcResponse {
    /// True when this response matches the numeric request `expected_id`.
    #[must_use]
    pub(crate) fn matches_id(&self, expected_id: u64) -> bool {
        match &self.id {
            Some(Value::Number(n)) => n.as_u64() == Some(expected_id),
            Some(Value::String(s)) => s.parse::<u64>().ok() == Some(expected_id),
            _ => false,
        }
    }
}

/// One tool descriptor returned by MCP `tools/list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct McpToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<Value>,
}

/// Decoded `tools/list` result payload.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct McpToolsListResult {
    #[serde(default)]
    pub tools: Vec<McpToolDescriptor>,
    #[serde(default, rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// One content block inside an MCP `tools/call` result.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct McpContentBlock {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub resource: Option<Value>,
}

/// Decoded `tools/call` result payload.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct McpToolCallPayload {
    #[serde(default)]
    pub content: Vec<McpContentBlock>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

/// Build the standard MCP `initialize` params object.
#[must_use]
pub(crate) fn initialize_params() -> Value {
    serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": {
            "name": "nexora",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

/// True when `segment` consists strictly of safe ASCII tool-identifier
/// characters (`[A-Za-z0-9_-]`), non-empty, and without `".."`.
#[must_use]
pub(crate) fn is_valid_identifier_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.contains("..")
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Format a namespaced MCP tool name (`mcp__<server>__<tool>`).
/// Returns `None` if either segment is unsafe, the server name contains `"__"`,
/// or the combined name exceeds [`MAX_NAMESPACED_TOOL_LEN`].
#[must_use]
pub(crate) fn format_mcp_tool_name(server: &str, tool: &str) -> Option<String> {
    if server.contains("__")
        || !is_valid_identifier_segment(server)
        || !is_valid_identifier_segment(tool)
    {
        return None;
    }
    let full = format!("{MCP_TOOL_PREFIX}{server}__{tool}");
    if full.len() > MAX_NAMESPACED_TOOL_LEN {
        return None;
    }
    Some(full)
}

/// Parse `mcp__<server>__<tool>` into `(server, tool)`.
///
/// Validates strict ASCII identifier grammar and length so untrusted model
/// tool names can never break out of prompt-injection envelopes or spoof
/// native tools.
#[must_use]
pub(crate) fn parse_mcp_tool_name(namespaced: &str) -> Option<(&str, &str)> {
    if namespaced.len() > MAX_NAMESPACED_TOOL_LEN {
        return None;
    }
    let rest = namespaced.strip_prefix(MCP_TOOL_PREFIX)?;
    let sep = rest.find("__")?;
    let server = &rest[..sep];
    let tool = &rest[sep + 2..];
    if server.contains("__")
        || !is_valid_identifier_segment(server)
        || !is_valid_identifier_segment(tool)
    {
        return None;
    }
    Some((server, tool))
}

/// Convert a discovered MCP tool descriptor into a sanitized, namespaced
/// [`ToolDefinition`] ready for the agent loop.
#[must_use]
pub(crate) fn descriptor_to_tool_definition(
    server_name: &str,
    desc: &McpToolDescriptor,
) -> Option<ToolDefinition> {
    let namespaced = format_mcp_tool_name(server_name, &desc.name)?;
    let raw_desc = desc.description.as_deref().unwrap_or("");
    let cleaned_desc: String = raw_desc
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TOOL_DESCRIPTION_LEN)
        .collect();
    let (redacted_desc, _) = redact_secrets(cleaned_desc.trim());
    let description = if redacted_desc.is_empty() {
        format!("MCP tool '{}' from server '{}'", desc.name, server_name)
    } else {
        format!("[MCP:{server_name}] {redacted_desc}")
    };

    let parameters = match &desc.input_schema {
        Some(Value::Object(obj)) => {
            let mut schema = obj.clone();
            schema
                .entry("type".to_string())
                .or_insert_with(|| Value::String("object".to_string()));
            Value::Object(schema)
        }
        _ => serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": true
        }),
    };

    Some(ToolDefinition {
        name: namespaced,
        description,
        parameters,
    })
}

/// Render an MCP `tools/call` response into a bounded, secret-redacted text
/// observation.
#[must_use]
pub(crate) fn format_tool_call_result(value: &Value) -> (String, bool) {
    let Ok(parsed) = serde_json::from_value::<McpToolCallPayload>(value.clone()) else {
        let raw = value.to_string();
        return (truncate_and_redact(&raw), false);
    };

    let mut parts = Vec::new();
    for block in &parsed.content {
        match block.kind.as_str() {
            "text" => {
                if let Some(text) = &block.text {
                    parts.push(text.clone());
                }
            }
            "image" => {
                let mime = block
                    .mime_type
                    .as_deref()
                    .unwrap_or("application/octet-stream");
                parts.push(format!("[image content: {mime}]"));
            }
            "resource" => {
                if let Some(res) = &block.resource {
                    if let Some(text) = res.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    } else if let Some(uri) = res.get("uri").and_then(Value::as_str) {
                        parts.push(format!("[resource: {uri}]"));
                    } else {
                        parts.push("[resource content]".to_string());
                    }
                }
            }
            other => {
                parts.push(format!("[{other} content]"));
            }
        }
    }

    let joined = if parts.is_empty() {
        "(empty tool output)".to_string()
    } else {
        parts.join("\n")
    };
    (truncate_and_redact(&joined), parsed.is_error)
}

fn truncate_and_redact(raw: &str) -> String {
    let (redacted, _) = redact_secrets(raw);
    if redacted.len() <= MAX_MCP_TOOL_OUTPUT_BYTES {
        return redacted;
    }
    let mut cut = MAX_MCP_TOOL_OUTPUT_BYTES;
    while cut > 0 && !redacted.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n... [truncated MCP output at {} bytes]",
        &redacted[..cut],
        MAX_MCP_TOOL_OUTPUT_BYTES
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespacing_round_trips_and_rejects_hostile_names() {
        assert_eq!(
            format_mcp_tool_name("github", "search_code").as_deref(),
            Some("mcp__github__search_code")
        );
        assert_eq!(
            parse_mcp_tool_name("mcp__github__search_code"),
            Some(("github", "search_code"))
        );
        // Tool names containing single or double underscores remain unambiguous
        // because server names cannot contain `__`.
        assert_eq!(
            parse_mcp_tool_name("mcp__srv__nested__tool"),
            Some(("srv", "nested__tool"))
        );
        assert!(format_mcp_tool_name("bad__srv", "tool").is_none());
        assert!(parse_mcp_tool_name("read_file").is_none());
        assert!(parse_mcp_tool_name("mcp____tool").is_none());
        assert!(parse_mcp_tool_name("mcp__srv__").is_none());
        assert!(parse_mcp_tool_name("mcp__srv__../evil").is_none());
        assert!(parse_mcp_tool_name("mcp__srv__bad name").is_none());
    }

    #[test]
    fn tool_output_redacts_secrets_and_truncates() {
        let val = serde_json::json!({
            "content": [{"type": "text", "text": "token=sk-live-supersecret1234567890"}],
            "isError": false
        });
        let (out, is_err) = format_tool_call_result(&val);
        assert!(!is_err);
        assert!(!out.contains("sk-live-supersecret1234567890"));

        let huge = "a".repeat(MAX_MCP_TOOL_OUTPUT_BYTES + 1024);
        let val2 = serde_json::json!({
            "content": [{"type": "text", "text": huge}],
            "isError": false
        });
        let (out2, _) = format_tool_call_result(&val2);
        assert!(out2.contains("truncated MCP output"));
    }
}
