//! Hardened `Streamable HTTP` MCP transport (MCP 2025-03-26):
//! - Uses `reqwest::blocking::Client` (`rustls`, redirects disabled, DNS
//!   pinned after SSRF validation).
//! - Validates endpoint URL (`https://` or explicit `localhost`/`127.0.0.1`/`[::1]`).
//! - Supports both `application/json` and `text/event-stream` (SSE) responses
//!   with hard size caps.
//! - Tracks `Mcp-Session-Id` across requests.
//! - Attaches optional `Authorization: Bearer <token>` from OS keyring (never
//!   logged or echoed in errors).

use std::io::Read;
use std::time::Duration;

use serde_json::Value;
use url::Url;

use crate::application::agent::control::CancellationToken;

use super::jsonrpc::{
    format_tool_call_result, initialize_params, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, McpToolDescriptor, McpToolsListResult, MAX_JSONRPC_PAYLOAD_BYTES,
    MAX_TOOLS_LIST_PAGES, MAX_TOOLS_PER_SERVER, MCP_PROTOCOL_VERSION,
};
use super::url_safety::{build_hardened_http_client, validate_mcp_url};
use super::McpError;

/// Default hard timeout per MCP Streamable HTTP round trip.
pub(crate) const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum accepted length of an `Mcp-Session-Id` header value.
const MAX_SESSION_ID_LEN: usize = 256;

/// Validated `Streamable HTTP` MCP client.
pub(crate) struct StreamableHttpClient {
    url: Url,
    client: reqwest::blocking::Client,
    bearer_token: Option<String>,
    session_id: Option<String>,
    next_id: u64,
}

impl StreamableHttpClient {
    /// Validate the endpoint URL, construct a hardened HTTP client, and run
    /// the MCP `initialize` + `notifications/initialized` handshake.
    ///
    /// # Errors
    /// Returns a secret-free [`McpError`].
    pub(crate) fn connect(
        raw_url: &str,
        bearer_token: Option<String>,
        timeout: Duration,
        token: &CancellationToken,
    ) -> Result<Self, McpError> {
        if token.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        let url =
            validate_mcp_url(raw_url).map_err(|reason| McpError::InvalidUrl(reason.message()))?;
        let client = build_hardened_http_client(&url, timeout)
            .map_err(|reason| McpError::InvalidUrl(reason.message()))?;

        let mut http_client = Self {
            url,
            client,
            bearer_token,
            session_id: None,
            next_id: 1,
        };
        http_client.initialize(token)?;
        Ok(http_client)
    }

    #[cfg(test)]
    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn apply_headers(
        &self,
        mut req: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        req = req
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION);
        if let Some(session_id) = &self.session_id {
            req = req.header("Mcp-Session-Id", session_id);
        }
        if let Some(bearer) = &self.bearer_token {
            req = req.bearer_auth(bearer);
        }
        req
    }

    fn capture_session_id(&mut self, headers: &reqwest::header::HeaderMap) {
        if let Some(val) = headers.get("mcp-session-id") {
            if let Ok(text) = val.to_str() {
                let trimmed = text.trim();
                if !trimmed.is_empty()
                    && trimmed.len() <= MAX_SESSION_ID_LEN
                    && trimmed.bytes().all(|b| (0x21..=0x7e).contains(&b))
                {
                    self.session_id = Some(trimmed.to_string());
                }
            }
        }
    }

    fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
        token: &CancellationToken,
    ) -> Result<(), McpError> {
        if token.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        let notif = JsonRpcNotification::new(method, params);
        let response = self
            .apply_headers(self.client.post(self.url.clone()))
            .json(&notif)
            .send()
            .map_err(|err| classify_reqwest_error(&err))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(McpError::Unauthorized);
        }
        self.capture_session_id(response.headers());
        if !status.is_success() {
            return Err(McpError::HttpStatus {
                status: status.as_u16(),
            });
        }
        Ok(())
    }

    fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
        token: &CancellationToken,
    ) -> Result<Value, McpError> {
        if token.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let req_payload = JsonRpcRequest::new(id, method, params);

        let response = self
            .apply_headers(self.client.post(self.url.clone()))
            .json(&req_payload)
            .send()
            .map_err(|err| classify_reqwest_error(&err))?;

        if token.is_cancelled() {
            return Err(McpError::Cancelled);
        }

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(McpError::Unauthorized);
        }
        self.capture_session_id(response.headers());
        if !status.is_success() {
            return Err(McpError::HttpStatus {
                status: status.as_u16(),
            });
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_ascii_lowercase();

        let mut body = Vec::new();
        response
            .take(MAX_JSONRPC_PAYLOAD_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| McpError::Transport("failed to read MCP HTTP response body"))?;
        if body.len() > MAX_JSONRPC_PAYLOAD_BYTES {
            return Err(McpError::ResponseTooLarge);
        }

        let resp = if content_type.contains("text/event-stream") {
            parse_sse_jsonrpc_response(&body, id)?
        } else {
            serde_json::from_slice::<JsonRpcResponse>(&body).map_err(|_| {
                McpError::Protocol("MCP HTTP server returned malformed JSON-RPC payload")
            })?
        };

        if let Some(err) = resp.error {
            return Err(McpError::JsonRpcError { code: err.code });
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    fn initialize(&mut self, token: &CancellationToken) -> Result<(), McpError> {
        let _ = self.request("initialize", Some(initialize_params()), token)?;
        self.send_notification("notifications/initialized", None, token)?;
        Ok(())
    }

    /// Discover tools via `tools/list` (with bounded cursor pagination).
    pub(crate) fn list_tools(
        &mut self,
        token: &CancellationToken,
    ) -> Result<Vec<McpToolDescriptor>, McpError> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOLS_LIST_PAGES {
            let params = cursor.as_ref().map(|c| serde_json::json!({ "cursor": c }));
            let raw = self.request("tools/list", params, token)?;
            let page: McpToolsListResult = serde_json::from_value(raw).map_err(|_| {
                McpError::Protocol("MCP server returned malformed tools/list result")
            })?;
            for tool in page.tools {
                if all.len() >= MAX_TOOLS_PER_SERVER {
                    return Ok(all);
                }
                all.push(tool);
            }
            match page.next_cursor.filter(|c| !c.is_empty()) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(all)
    }

    /// Invoke one tool via `tools/call`, returning `(bounded_output, is_error)`.
    pub(crate) fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        token: &CancellationToken,
    ) -> Result<(String, bool), McpError> {
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments,
        });
        let raw = self.request("tools/call", Some(params), token)?;
        Ok(format_tool_call_result(&raw))
    }
}

fn classify_reqwest_error(err: &reqwest::Error) -> McpError {
    if err.is_timeout() {
        McpError::Timeout
    } else {
        McpError::Transport("MCP Streamable HTTP request failed")
    }
}

/// Parse an SSE (`text/event-stream`) body and return the JSON-RPC response
/// matching `expected_id`. Supports multi-line `data:` events and skips
/// comments/notifications.
pub(crate) fn parse_sse_jsonrpc_response(
    body: &[u8],
    expected_id: u64,
) -> Result<JsonRpcResponse, McpError> {
    let text = std::str::from_utf8(body)
        .map_err(|_| McpError::Protocol("MCP SSE response is not valid UTF-8"))?;

    let mut data_lines: Vec<&str> = Vec::new();
    let mut fallback: Option<JsonRpcResponse> = None;

    let mut flush_event = |lines: &mut Vec<&str>| -> Option<JsonRpcResponse> {
        if lines.is_empty() {
            return None;
        }
        let joined = lines.join("\n");
        lines.clear();
        let trimmed = joined.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return None;
        }
        let parsed = serde_json::from_str::<JsonRpcResponse>(trimmed).ok()?;
        if parsed.matches_id(expected_id) {
            return Some(parsed);
        }
        if fallback.is_none() && (parsed.result.is_some() || parsed.error.is_some()) {
            fallback = Some(parsed);
        }
        None
    };

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            if let Some(matched) = flush_event(&mut data_lines) {
                return Ok(matched);
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let value = rest.strip_prefix(' ').unwrap_or(rest);
            data_lines.push(value);
        }
    }

    if let Some(matched) = flush_event(&mut data_lines) {
        return Ok(matched);
    }
    fallback.ok_or(McpError::Protocol(
        "MCP SSE stream ended without a matching JSON-RPC response",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn sse_parser_extracts_matching_id_and_skips_notifications() {
        let sse = concat!(
            ": keepalive\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":2,\n",
            "data: \"result\":{\"tools\":[]}}\n\n"
        );
        let resp = parse_sse_jsonrpc_response(sse.as_bytes(), 2).expect("parse sse");
        assert!(resp.matches_id(2));
        assert!(resp.result.is_some());
    }

    #[test]
    fn streamable_http_tracks_session_id_supports_json_and_sse_and_bearer() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock http mcp");
        let port = listener.local_addr().unwrap().port();

        let server = std::thread::spawn(move || {
            for step in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                assert!(
                    req.contains("Authorization: Bearer top-secret-bearer")
                        || req.contains("authorization: Bearer top-secret-bearer"),
                    "missing bearer header on step {step}: {req}"
                );
                if step > 0 {
                    assert!(
                        req.to_ascii_lowercase()
                            .contains("mcp-session-id: sess-42-xyz"),
                        "missing Mcp-Session-Id on step {step}: {req}"
                    );
                }
                let resp = match step {
                    0 => {
                        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: sess-42-xyz\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    }
                    1 => "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string(),
                    2 => {
                        // Respond with text/event-stream (SSE) for tools/list!
                        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"ping\",\"description\":\"Ping tool\",\"inputSchema\":{\"type\":\"object\"}}]}}\n\n";
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    }
                    _ => {
                        let body = r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"pong-ok"}],"isError":false}}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    }
                };
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let token = CancellationToken::new();
        let url = format!("http://127.0.0.1:{port}/mcp");
        let mut client = StreamableHttpClient::connect(
            &url,
            Some("top-secret-bearer".to_string()),
            Duration::from_secs(5),
            &token,
        )
        .expect("connect streamable http");
        assert_eq!(client.session_id(), Some("sess-42-xyz"));

        let tools = client.list_tools(&token).expect("list tools via SSE");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "ping");

        let (out, is_err) = client
            .call_tool("ping", &serde_json::json!({}), &token)
            .expect("call tool via JSON");
        assert!(!is_err);
        assert_eq!(out, "pong-ok");

        server.join().unwrap();
    }

    #[test]
    fn streamable_http_rejects_ssrf_urls() {
        let token = CancellationToken::new();
        let err = StreamableHttpClient::connect(
            "http://169.254.169.254/latest/meta-data",
            None,
            Duration::from_secs(2),
            &token,
        )
        .err()
        .expect("metadata IP must be rejected");
        assert!(matches!(err, McpError::InvalidUrl(_)));
    }
}
