use std::time::Duration;

use nexora_core::provider::{
    AiAttachmentPayload, AiRequest, AiResponse, AiRole, CancellationToken, ExecutorError,
    ProviderExecutor, ReasoningEffort, StreamEvent, TokenUsage, ToolCall,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::chatgpt_oauth;
use super::transport::{Credential, HttpClient, PostRequest, StreamOutcome, StreamPostError};

pub(crate) const PROVIDER_NAME: &str = "chatgpt_subscription";
pub(crate) const PROVIDER_DISPLAY_NAME: &str = "ChatGPT subscription";

const ENDPOINT: &str = "https://api.openai.com/v1/responses";
const TOOL_NAMESPACE: &str = "nexora";

pub(crate) struct ChatGptPlanExecutor {
    http: Option<HttpClient>,
}

impl ChatGptPlanExecutor {
    pub(crate) fn new() -> Self {
        Self {
            http: HttpClient::single_stream_attempt(),
        }
    }

    fn send(
        &self,
        endpoint: &str,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
        on_event: &dyn Fn(StreamEvent),
    ) -> Result<AiResponse, ExecutorError> {
        if credential.is_empty() {
            return Err(ExecutorError::Authentication);
        }
        let http = self.http.as_ref().ok_or(ExecutorError::Network)?;
        let body = serde_json::to_vec(&request_body(request)?)
            .map_err(|_| ExecutorError::InvalidRequest)?;
        let exchange = http.post_stream(
            token,
            &PostRequest {
                url: endpoint.to_owned(),
                credential: Credential::Bearer(credential),
                extra_headers: &[("Accept", "text/event-stream")],
                extra_headers_owned: &[],
                body: &body,
                timeout: Some(request.request_timeout.unwrap_or(Duration::from_mins(2))),
            },
            &mut |frame| {
                let event = serde_json::from_str(&frame.data)
                    .map_err(|_| StreamEnd::Failed(ExecutorError::UnexpectedResponse))?;
                match event {
                    PlanEvent::Text { delta } => on_event(StreamEvent::TextDelta(delta)),
                    PlanEvent::Completed { response } => {
                        let result = response.into_response(request).map_err(StreamEnd::Failed)?;
                        return Err(StreamEnd::Completed(result));
                    }
                    PlanEvent::Failed { response } => {
                        return Err(StreamEnd::Failed(classify_failure(response.error.as_ref())));
                    }
                    PlanEvent::Incomplete | PlanEvent::Error => {
                        return Err(StreamEnd::Failed(ExecutorError::UnexpectedResponse));
                    }
                    PlanEvent::Other => {}
                }
                Ok(())
            },
        );
        match exchange {
            Err(StreamPostError::Handler(StreamEnd::Completed(response))) => Ok(response),
            Err(
                StreamPostError::Handler(StreamEnd::Failed(error))
                | StreamPostError::Transport(error),
            ) => Err(error),
            Ok(StreamOutcome::Completed) => Err(ExecutorError::UnexpectedResponse),
            Ok(StreamOutcome::Failure(error)) => Err(super::transport::map_http_error(
                error.status,
                error.retry_after_secs,
                error.is_context_length_exceeded,
            )),
        }
    }
}

enum StreamEnd {
    Completed(AiResponse),
    Failed(ExecutorError),
}

impl ProviderExecutor for ChatGptPlanExecutor {
    fn execute(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
    ) -> Result<AiResponse, ExecutorError> {
        self.execute_stream_events(request, credential, token, &|_| {})
    }

    fn execute_stream_events(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
        on_event: &dyn Fn(StreamEvent),
    ) -> Result<AiResponse, ExecutorError> {
        if request.provider != PROVIDER_NAME {
            return Err(ExecutorError::InvalidRequest);
        }
        if token.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        let call =
            chatgpt_oauth::authorize_call(credential, &request.model, token).map_err(|err| {
                match err {
                    chatgpt_oauth::ChatGptOAuthError::TokenRefreshTransient => {
                        ExecutorError::ProviderUnavailable
                    }
                    _ => ExecutorError::Authentication,
                }
            })?;
        self.send(ENDPOINT, request, &call.access_token, token, on_event)
    }
}

fn push_tool_calls(input: &mut Vec<Value>, calls: &[ToolCall]) {
    for call in calls {
        input.push(json!({
            "type": "function_call",
            "namespace": TOOL_NAMESPACE,
            "call_id": call.id,
            "name": call.name,
            "arguments": call.arguments,
        }));
    }
}

fn encode_input_message(
    input: &mut Vec<Value>,
    message: &nexora_core::provider::AiMessage,
) -> Result<(), ExecutorError> {
    let role = match message.role {
        AiRole::System => "developer",
        AiRole::User => "user",
        AiRole::Assistant => {
            if message
                .attachments
                .iter()
                .any(|attachment| matches!(attachment.payload, AiAttachmentPayload::Base64(_)))
            {
                return Err(ExecutorError::InvalidRequest);
            }
            let text = message.composed_content();
            if !text.is_empty() {
                input.push(json!({ "role": "assistant", "content": text }));
            }
            push_tool_calls(input, &message.tool_calls);
            return Ok(());
        }
        AiRole::Tool => {
            let result = message
                .tool_result
                .as_ref()
                .ok_or(ExecutorError::InvalidRequest)?;
            input.push(json!({
                "type": "function_call_output",
                "call_id": result.call_id,
                "output": result.content,
            }));
            return Ok(());
        }
    };
    let mut content = Vec::new();
    let text = message.composed_content();
    if !text.is_empty() {
        content.push(json!({ "type": "input_text", "text": text }));
    }
    for attachment in &message.attachments {
        if let AiAttachmentPayload::Base64(data) = &attachment.payload {
            if message.role != AiRole::User {
                return Err(ExecutorError::InvalidRequest);
            }
            let mime = attachment
                .mime_type
                .as_deref()
                .ok_or(ExecutorError::InvalidRequest)?;
            let data_url = format!("data:{mime};base64,{data}");
            if matches!(
                mime,
                "image/png" | "image/jpeg" | "image/webp" | "image/gif"
            ) {
                content.push(json!({ "type": "input_image", "image_url": data_url }));
            } else if mime == "application/pdf" {
                content.push(json!({
                    "type": "input_file",
                    "filename": attachment.file_name,
                    "file_data": data_url,
                }));
            } else {
                return Err(ExecutorError::InvalidRequest);
            }
        }
    }
    if !content.is_empty() {
        input.push(json!({ "role": role, "content": content }));
    }
    push_tool_calls(input, &message.tool_calls);
    Ok(())
}

fn request_body(request: &AiRequest) -> Result<Value, ExecutorError> {
    let mut input = Vec::new();
    for message in request.messages.iter() {
        encode_input_message(&mut input, message)?;
    }
    let mut payload =
        json!({ "model": request.model, "input": input, "store": false, "stream": true });
    if let Some(config) = &request.model_config {
        if config.temperature.is_some() || config.max_tokens.is_some() {
            return Err(ExecutorError::InvalidRequest);
        }
        if let Some(effort) = config.effort.as_deref() {
            let effort = ReasoningEffort::parse(effort).ok_or(ExecutorError::InvalidRequest)?;
            payload["reasoning"] = json!({"effort": effort.as_str()});
        }
    }
    if !request.tools.is_empty() {
        let tools: Vec<_> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                    "strict": false,
                })
            })
            .collect();
        payload["tools"] = json!([{
            "type": "namespace", "name": TOOL_NAMESPACE,
            "description": "Tools provided by Nexora", "tools": tools,
        }]);
    }
    Ok(payload)
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum PlanEvent {
    #[serde(rename = "response.output_text.delta")]
    Text { delta: String },
    #[serde(rename = "response.completed")]
    Completed { response: CompletedTurn },
    #[serde(rename = "response.failed")]
    Failed { response: FailedTurn },
    #[serde(rename = "response.incomplete")]
    Incomplete,
    #[serde(rename = "error")]
    Error,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct FailedTurn {
    error: Option<PlanError>,
}

#[derive(Deserialize)]
struct PlanError {
    code: String,
}

fn classify_failure(error: Option<&PlanError>) -> ExecutorError {
    match error.map(|error| error.code.as_str()) {
        Some("subscription_sharing_usage_limit_exceeded") => ExecutorError::RateLimited {
            retry_after_secs: None,
        },
        Some("subscription_sharing_usage_unavailable") => ExecutorError::ProviderUnavailable,
        Some("chatpass_v2_scope_not_authorized" | "chatpass_v2_invalid_authorization_context") => {
            ExecutorError::Authentication
        }
        Some("context_length_exceeded") => ExecutorError::ContextLengthExceeded,
        _ => ExecutorError::Failure,
    }
}

#[derive(Deserialize)]
struct CompletedTurn {
    model: String,
    output: Vec<OutputItem>,
    usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutputItem {
    Message {
        content: Vec<OutputContent>,
    },
    FunctionCall {
        namespace: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutputContent {
    OutputText {
        text: String,
    },
    Refusal {
        refusal: String,
    },
    #[serde(other)]
    Other,
}

impl CompletedTurn {
    fn into_response(self, request: &AiRequest) -> Result<AiResponse, ExecutorError> {
        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        for item in self.output {
            match item {
                OutputItem::Message { content } => {
                    for part in content {
                        match part {
                            OutputContent::OutputText { text: chunk } => text.push_str(&chunk),
                            OutputContent::Refusal { refusal } => text.push_str(&refusal),
                            OutputContent::Other => {}
                        }
                    }
                }
                OutputItem::FunctionCall {
                    namespace,
                    call_id,
                    name,
                    arguments,
                } => {
                    if namespace.as_deref() != Some(TOOL_NAMESPACE)
                        || call_id.is_empty()
                        || calls.iter().any(|call| call.id == call_id)
                        || !request.tools.iter().any(|tool| tool.name == name)
                    {
                        return Err(ExecutorError::UnexpectedResponse);
                    }
                    calls.push(ToolCall {
                        id: call_id,
                        name,
                        arguments,
                        thought_signature: None,
                    });
                }
                OutputItem::Other => {}
            }
        }
        if text.is_empty() && calls.is_empty() {
            return Err(ExecutorError::UnexpectedResponse);
        }
        Ok(AiResponse {
            content: text,
            model: self.model,
            tool_calls: calls,
            usage: self.usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexora_core::provider::AiMessage;
    use std::fmt::Write as _;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn fixture_request() -> AiRequest {
        AiRequest {
            provider: PROVIDER_NAME.to_owned(),
            model: "fixture-model".to_owned(),
            messages: Arc::new(vec![AiMessage {
                role: AiRole::User,
                content: "Synthetic input".to_owned(),
                attachments: vec![],
                tool_calls: vec![],
                tool_result: None,
            }]),
            tools: Arc::new(vec![]),
            model_config: None,
            request_timeout: Some(Duration::from_secs(5)),
        }
    }

    fn fixture_sse(events: Vec<Value>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "POST /v1/responses HTTP/1.1");
            let mut header = String::new();
            let mut saw_auth = false;
            let mut body_length = 0;
            loop {
                header.clear();
                assert!(reader.read_line(&mut header).unwrap() > 0);
                if header.trim().is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("authorization") {
                    assert_eq!(value.trim(), "Bearer fixture-plan-bearer");
                    saw_auth = true;
                }
                if name.eq_ignore_ascii_case("content-length") {
                    body_length = value.trim().parse().unwrap();
                }
            }
            assert!(saw_auth);
            let mut body = vec![0; body_length];
            reader.read_exact(&mut body).unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["model"], "fixture-model");
            assert_eq!(value["store"], false);
            assert_eq!(value["stream"], true);
            assert_eq!(value["input"][0]["content"][0]["text"], "Synthetic input");
            assert!(value.get("temperature").is_none());
            let mut wire = String::new();
            for event in events {
                write!(wire, "data: {event}\n\n").unwrap();
            }
            write!(socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{wire}",
                wire.len(),
            ).unwrap();
        });
        (format!("http://{address}/v1/responses"), handle)
    }

    #[test]
    fn subscription_bearer_and_context_use_responses_and_require_completion() {
        assert_eq!(ENDPOINT, "https://api.openai.com/v1/responses");
        let delta = json!({"type": "response.output_text.delta", "delta": "Benign result"});
        let terminal = json!({
            "type":"response.completed",
            "response":{
                "model":"fixture-model",
                "output":[{"type":"message","content":[{"type":"output_text","text":"Benign result"}]}],
            }
        });
        let (url, server) = fixture_sse(vec![delta.clone(), terminal]);
        let events = Mutex::new(Vec::new());
        let executor = ChatGptPlanExecutor::new();
        let result = executor
            .send(
                &url,
                &fixture_request(),
                "fixture-plan-bearer",
                &CancellationToken::new(),
                &|event| events.lock().unwrap().push(event),
            )
            .unwrap();
        server.join().unwrap();
        assert_eq!(result.content, "Benign result");
        assert_eq!(
            events.into_inner().unwrap(),
            vec![StreamEvent::TextDelta("Benign result".into())]
        );

        let (url, server) = fixture_sse(vec![delta]);
        assert!(matches!(
            executor.send(
                &url,
                &fixture_request(),
                "fixture-plan-bearer",
                &CancellationToken::new(),
                &|_| {},
            ),
            Err(ExecutorError::UnexpectedResponse)
        ));
        server.join().unwrap();
    }

    fn capture_wire_sse(
        reply: &str,
    ) -> (
        String,
        Arc<Mutex<Option<Value>>>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(None::<Value>));
        let captured_srv = Arc::clone(&captured);
        let reply = reply.to_owned();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "POST /v1/responses HTTP/1.1");
            let mut header = String::new();
            let mut body_length = 0;
            loop {
                header.clear();
                assert!(reader.read_line(&mut header).unwrap() > 0);
                if header.trim().is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    body_length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; body_length];
            reader.read_exact(&mut body).unwrap();
            *captured_srv.lock().unwrap() = Some(serde_json::from_slice(&body).unwrap());
            let terminal = json!({
                "type": "response.completed",
                "response": {
                    "model": "fixture-model",
                    "output": [{
                        "type": "message",
                        "content": [{"type": "output_text", "text": reply}]
                    }]
                }
            });
            let wire = format!("data: {terminal}\n\n");
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{wire}",
                wire.len(),
            )
            .unwrap();
        });
        (format!("http://{address}/v1/responses"), captured, server)
    }

    fn text_message(role: AiRole, content: &str) -> AiMessage {
        AiMessage {
            role,
            content: content.to_owned(),
            attachments: vec![],
            tool_calls: vec![],
            tool_result: None,
        }
    }

    #[test]
    fn second_turn_encodes_assistant_history_as_string_not_output_text_parts() {
        let (url, captured, server) = capture_wire_sse("Second turn reply");
        let second_turn = AiRequest {
            provider: PROVIDER_NAME.to_owned(),
            model: "fixture-model".to_owned(),
            messages: Arc::new(vec![
                text_message(AiRole::System, "System instruction"),
                text_message(AiRole::User, "First question"),
                text_message(AiRole::Assistant, "First assistant reply"),
                text_message(AiRole::User, "Follow-up question"),
            ]),
            tools: Arc::new(vec![]),
            model_config: None,
            request_timeout: Some(Duration::from_secs(5)),
        };

        let executor = ChatGptPlanExecutor::new();
        let response = executor
            .send(
                &url,
                &second_turn,
                "fixture-plan-bearer",
                &CancellationToken::new(),
                &|_| {},
            )
            .expect("second turn succeeds");
        server.join().unwrap();
        assert_eq!(response.content, "Second turn reply");

        let wire_body = captured.lock().unwrap().take().expect("captured payload");
        let input = wire_body["input"].as_array().expect("input array");
        assert_eq!(input.len(), 4);
        assert_eq!(
            input[0],
            json!({
                "role": "developer",
                "content": [{"type": "input_text", "text": "System instruction"}]
            })
        );
        assert_eq!(
            input[1],
            json!({
                "role": "user",
                "content": [{"type": "input_text", "text": "First question"}]
            })
        );
        assert_eq!(
            input[2],
            json!({
                "role": "assistant",
                "content": "First assistant reply"
            })
        );
        assert!(
            input[2]["content"].is_string(),
            "assistant history inside easy input message must be a string, not output_text parts"
        );
        assert_eq!(
            input[3],
            json!({
                "role": "user",
                "content": [{"type": "input_text", "text": "Follow-up question"}]
            })
        );
    }
}
