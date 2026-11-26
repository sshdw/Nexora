//! Zero-token provider API metadata fetcher & parser (`M06` / P10).
//!
//! Queries provider `/models` metadata endpoints over HTTP `GET` without
//! generating any completion tokens, extracting context window, reasoning/thinking
//! capability, tool support, and modality/attachment flags per model id.
//!
//! All HTTP errors are mapped to secret-free categories; credentials and raw
//! URLs/error bodies never leave this module.

use std::time::Duration;

use nexora_core::provider::{CancellationToken, ExecutorError};
use serde::{Deserialize, Serialize};

use super::catalog::openai::find_preset;
use super::transport::{Credential, HttpClient, PostOutcome, TransportProbeError};

pub(crate) const METADATA_TIMEOUT: Duration = Duration::from_secs(10);

/// Capability fields discovered from a provider's zero-token `/models` endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ApiModelMetadata {
    /// Upstream model identifier.
    pub model_id: String,
    /// Reported input/context window in tokens, when exposed by the API.
    pub context_window: Option<u32>,
    /// Reported maximum output/completion tokens, when exposed by the API.
    pub max_output_tokens: Option<u32>,
    /// Supported reasoning efforts inferred from API metadata parameters, if any.
    pub supported_efforts: Option<Vec<String>>,
    /// Whether the API metadata explicitly advertises thinking/reasoning support.
    pub supports_thinking: Option<bool>,
    /// Whether the API metadata explicitly advertises image/file/multimodal input.
    pub supports_attachments: Option<bool>,
    /// Whether the API metadata explicitly advertises tool/function calling.
    pub supports_tools: Option<bool>,
}

impl ApiModelMetadata {
    /// True when the API response supplied at least one concrete capability
    /// field (not a thin `{ id }` listing where every field is `None`).
    #[must_use]
    pub(crate) fn has_any_field(&self) -> bool {
        self.context_window.is_some()
            || self.max_output_tokens.is_some()
            || self.supported_efforts.is_some()
            || self.supports_thinking.is_some()
            || self.supports_attachments.is_some()
            || self.supports_tools.is_some()
    }
}

/// Secret-free outcome error for connection diagnostic probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataProbeError {
    AuthFailed,
    Timeout,
    DnsOrProxy,
    TlsError,
    NotRunning,
    HttpStatus(u16),
    UnexpectedResponse,
    InvalidConfig,
}

impl From<TransportProbeError> for MetadataProbeError {
    fn from(err: TransportProbeError) -> Self {
        match err {
            TransportProbeError::Timeout => Self::Timeout,
            TransportProbeError::TlsError => Self::TlsError,
            TransportProbeError::NotRunning => Self::NotRunning,
            TransportProbeError::DnsOrProxy | TransportProbeError::Cancelled => Self::DnsOrProxy,
        }
    }
}

/// Derive an OpenAI-style `/models` URL from a `/chat/completions` or base URL.
#[must_use]
pub(crate) fn openai_models_url(chat_or_base_url: &str) -> String {
    let trimmed = chat_or_base_url.trim().trim_end_matches('/');
    if let Some(prefix) = trimmed.strip_suffix("/chat/completions") {
        format!("{prefix}/models")
    } else if trimmed.ends_with("/models") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/models")
    }
}

/// Fetch zero-token model metadata from the provider's `/models` endpoint.
///
/// `compat_chat_url` is required when `connection_id == "openai_compat"`.
pub(crate) fn fetch_connection_metadata(
    connection_id: &str,
    credential: &str,
    compat_chat_url: Option<&str>,
    compat_headers: &[(String, String)],
) -> Result<Vec<ApiModelMetadata>, ExecutorError> {
    let client = HttpClient::new();
    let token = CancellationToken::new();
    match connection_id {
        "anthropic" => {
            let outcome = client.get_json(
                &token,
                "https://api.anthropic.com/v1/models",
                Some(Credential::Header {
                    name: "x-api-key",
                    value: credential,
                }),
                &[("anthropic-version", "2023-06-01")],
                &[],
                Some(METADATA_TIMEOUT),
            )?;
            handle_get_outcome(outcome, parse_anthropic_models_metadata)
        }
        "gemini" => {
            let outcome = client.get_json(
                &token,
                "https://generativelanguage.googleapis.com/v1beta/models",
                Some(Credential::Header {
                    name: "x-goog-api-key",
                    value: credential,
                }),
                &[],
                &[],
                Some(METADATA_TIMEOUT),
            )?;
            handle_get_outcome(outcome, parse_gemini_models_metadata)
        }
        _ => {
            let (endpoint, preset_headers): (String, &[(&str, &str)]) = match connection_id {
                "openai" => ("https://api.openai.com/v1/models".to_string(), &[]),
                "openai_compat" => match compat_chat_url {
                    Some(url) if !url.trim().is_empty() => (openai_models_url(url), &[]),
                    _ => return Err(ExecutorError::InvalidRequest),
                },
                other => match find_preset(other) {
                    Some(preset) => (openai_models_url(preset.endpoint), preset.extra_headers),
                    None => return Err(ExecutorError::InvalidRequest),
                },
            };
            let outcome = client.get_json(
                &token,
                &endpoint,
                Some(Credential::Bearer(credential)),
                preset_headers,
                compat_headers,
                Some(METADATA_TIMEOUT),
            )?;
            handle_get_outcome(outcome, parse_openai_models_metadata)
        }
    }
}

fn handle_get_outcome(
    outcome: PostOutcome,
    parser: fn(&[u8]) -> Result<Vec<ApiModelMetadata>, ExecutorError>,
) -> Result<Vec<ApiModelMetadata>, ExecutorError> {
    match outcome {
        PostOutcome::Success(bytes) => parser(&bytes),
        PostOutcome::Failure(snapshot) => Err(super::transport::map_http_error(
            snapshot.status,
            snapshot.retry_after_secs,
            snapshot.is_context_length_exceeded,
        )),
    }
}

fn handle_probe_outcome(
    outcome: PostOutcome,
    parser: fn(&[u8]) -> Result<Vec<ApiModelMetadata>, ExecutorError>,
) -> Result<Vec<ApiModelMetadata>, MetadataProbeError> {
    match outcome {
        PostOutcome::Success(bytes) => {
            parser(&bytes).map_err(|_| MetadataProbeError::UnexpectedResponse)
        }
        PostOutcome::Failure(snapshot) => match snapshot.status {
            401 | 403 => Err(MetadataProbeError::AuthFailed),
            status => Err(MetadataProbeError::HttpStatus(status)),
        },
    }
}

/// Probe a provider's zero-token `/models` endpoint with a 10s connect and
/// request timeout, classifying every transport/HTTP failure into a secret-free
/// [`MetadataProbeError`].
pub(crate) fn probe_connection_metadata(
    connection_id: &str,
    credential: Option<&str>,
    compat_chat_url: Option<&str>,
    compat_headers: &[(String, String)],
) -> Result<Vec<ApiModelMetadata>, MetadataProbeError> {
    let client = HttpClient::new();
    let token = CancellationToken::new();
    match connection_id {
        "anthropic" => {
            let cred = credential.ok_or(MetadataProbeError::AuthFailed)?;
            let outcome = client.get_json_probe(
                &token,
                "https://api.anthropic.com/v1/models",
                Some(Credential::Header {
                    name: "x-api-key",
                    value: cred,
                }),
                &[("anthropic-version", "2023-06-01")],
                &[],
                METADATA_TIMEOUT,
            )?;
            handle_probe_outcome(outcome, parse_anthropic_models_metadata)
        }
        "gemini" => {
            let cred = credential.ok_or(MetadataProbeError::AuthFailed)?;
            let outcome = client.get_json_probe(
                &token,
                "https://generativelanguage.googleapis.com/v1beta/models",
                Some(Credential::Header {
                    name: "x-goog-api-key",
                    value: cred,
                }),
                &[],
                &[],
                METADATA_TIMEOUT,
            )?;
            handle_probe_outcome(outcome, parse_gemini_models_metadata)
        }
        _ => {
            let (endpoint, preset_headers): (String, &[(&str, &str)]) = match connection_id {
                "openai" => ("https://api.openai.com/v1/models".to_string(), &[]),
                "openai_compat" => match compat_chat_url {
                    Some(url) if !url.trim().is_empty() => (openai_models_url(url), &[]),
                    _ => return Err(MetadataProbeError::InvalidConfig),
                },
                other => match find_preset(other) {
                    Some(preset) => (openai_models_url(preset.endpoint), preset.extra_headers),
                    None => return Err(MetadataProbeError::InvalidConfig),
                },
            };
            let bearer = credential.map(Credential::Bearer);
            if connection_id != "openai_compat" && bearer.is_none() {
                return Err(MetadataProbeError::AuthFailed);
            }
            let outcome = client.get_json_probe(
                &token,
                &endpoint,
                bearer,
                preset_headers,
                compat_headers,
                METADATA_TIMEOUT,
            )?;
            handle_probe_outcome(outcome, parse_openai_models_metadata)
        }
    }
}

#[derive(Deserialize)]
struct OpenAiModelsRoot {
    #[serde(default)]
    data: Vec<OpenAiModelEntry>,
}

#[derive(Deserialize)]
struct OpenAiTopProvider {
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    max_completion_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct OpenAiModelLimit {
    #[serde(default)]
    context: Option<u32>,
    #[serde(default)]
    output: Option<u32>,
}

#[derive(Deserialize)]
struct OpenAiCapabilityInput {
    #[serde(default)]
    image: Option<bool>,
    #[serde(default)]
    pdf: Option<bool>,
    #[serde(default)]
    audio: Option<bool>,
}

#[derive(Deserialize)]
struct OpenAiModelEntry {
    id: String,
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    context_window: Option<u32>,
    #[serde(default)]
    max_context_len: Option<u32>,
    #[serde(default)]
    max_context_length: Option<u32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    max_completion_tokens: Option<u32>,
    #[serde(default)]
    top_provider: Option<OpenAiTopProvider>,
    #[serde(default)]
    limit: Option<OpenAiModelLimit>,
    #[serde(default)]
    supported_parameters: Vec<String>,
    #[serde(default)]
    supported_efforts: Option<Vec<String>>,
    #[serde(default)]
    architecture: Option<OpenAiModelArchitecture>,
    #[serde(default)]
    capabilities: Option<OpenAiModelCapabilities>,
}

#[derive(Deserialize)]
struct OpenAiModelArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
}

#[derive(Deserialize)]
struct OpenAiModelCapabilities {
    #[serde(default)]
    thinking: Option<bool>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    tools: Option<bool>,
    #[serde(default)]
    function_calling: Option<bool>,
    #[serde(default)]
    toolcall: Option<bool>,
    #[serde(default)]
    vision: Option<bool>,
    #[serde(default)]
    attachments: Option<bool>,
    #[serde(default)]
    attachment: Option<bool>,
    #[serde(default)]
    input: Option<OpenAiCapabilityInput>,
    #[serde(default)]
    supported_efforts: Option<Vec<String>>,
}

impl OpenAiModelCapabilities {
    fn input_supports_attachments(&self) -> Option<bool> {
        let inp = self.input.as_ref()?;
        let flags = [inp.image, inp.pdf, inp.audio];
        if flags.contains(&Some(true)) {
            Some(true)
        } else if flags.iter().any(Option::is_some) {
            Some(false)
        } else {
            None
        }
    }
}

fn map_openai_entry(entry: OpenAiModelEntry) -> ApiModelMetadata {
    let context_window = entry
        .context_length
        .or(entry.context_window)
        .or(entry.max_context_len)
        .or(entry.max_context_length)
        .or_else(|| entry.limit.as_ref().and_then(|l| l.context))
        .or_else(|| entry.top_provider.as_ref().and_then(|tp| tp.context_length));
    let max_output_tokens = entry
        .max_output_tokens
        .or(entry.max_completion_tokens)
        .or_else(|| {
            entry
                .top_provider
                .as_ref()
                .and_then(|tp| tp.max_completion_tokens)
        })
        .or_else(|| entry.limit.as_ref().and_then(|l| l.output));
    let params_lower: Vec<String> = entry
        .supported_parameters
        .iter()
        .map(|p| p.to_ascii_lowercase())
        .collect();
    let supports_tools = entry
        .capabilities
        .as_ref()
        .and_then(|c| c.tools.or(c.function_calling).or(c.toolcall))
        .or_else(|| (!params_lower.is_empty()).then(|| params_lower.iter().any(|p| p == "tools")));
    let supports_thinking = entry
        .capabilities
        .as_ref()
        .and_then(|c| c.thinking.or(c.reasoning))
        .or_else(|| {
            (!params_lower.is_empty()).then(|| {
                params_lower.iter().any(|p| {
                    p == "reasoning" || p == "reasoning_effort" || p == "include_reasoning"
                })
            })
        });
    let supports_attachments = entry
        .capabilities
        .as_ref()
        .and_then(|c| {
            c.attachments
                .or(c.attachment)
                .or(c.vision)
                .or_else(|| c.input_supports_attachments())
        })
        .or_else(|| {
            entry.architecture.as_ref().map(|arch| {
                arch.input_modalities.iter().any(|m| {
                    matches!(
                        m.to_ascii_lowercase().as_str(),
                        "image" | "file" | "pdf" | "audio"
                    )
                })
            })
        });
    let supported_efforts = entry
        .supported_efforts
        .or_else(|| {
            entry
                .capabilities
                .as_ref()
                .and_then(|c| c.supported_efforts.clone())
        })
        .or_else(|| {
            if supports_thinking == Some(true)
                || params_lower
                    .iter()
                    .any(|p| p == "reasoning_effort" || p == "reasoning")
            {
                Some(vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                ])
            } else if supports_thinking == Some(false) {
                Some(Vec::new())
            } else {
                None
            }
        });
    ApiModelMetadata {
        model_id: entry.id,
        context_window,
        max_output_tokens,
        supported_efforts,
        supports_thinking,
        supports_attachments,
        supports_tools,
    }
}

/// Parse `OpenAI` / `OpenRouter` / `Mistral` / `OpenCode Zen` / vLLM / compat `/v1/models` JSON payloads.
pub(crate) fn parse_openai_models_metadata(
    bytes: &[u8],
) -> Result<Vec<ApiModelMetadata>, ExecutorError> {
    let parsed: OpenAiModelsRoot =
        serde_json::from_slice(bytes).map_err(|_| ExecutorError::UnexpectedResponse)?;
    Ok(parsed
        .data
        .into_iter()
        .filter(|e| !e.id.trim().is_empty())
        .map(map_openai_entry)
        .collect())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CapabilityFlag {
    Bool(bool),
    Object {
        #[serde(default)]
        supported: Option<bool>,
    },
}

impl CapabilityFlag {
    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            Self::Object { supported } => *supported,
        }
    }
}

#[derive(Deserialize)]
struct AnthropicEffortCapabilities {
    #[serde(default)]
    supported: Option<bool>,
    #[serde(default)]
    low: Option<CapabilityFlag>,
    #[serde(default)]
    medium: Option<CapabilityFlag>,
    #[serde(default)]
    high: Option<CapabilityFlag>,
    #[serde(default)]
    xhigh: Option<CapabilityFlag>,
    #[serde(default)]
    max: Option<CapabilityFlag>,
}

impl AnthropicEffortCapabilities {
    fn to_efforts(&self) -> Option<Vec<String>> {
        if self.supported == Some(false) {
            return Some(Vec::new());
        }
        let mut efforts = Vec::new();
        for (name, flag) in [
            ("low", self.low.as_ref()),
            ("medium", self.medium.as_ref()),
            ("high", self.high.as_ref()),
            ("xhigh", self.xhigh.as_ref().or(self.max.as_ref())),
        ] {
            if flag.and_then(CapabilityFlag::as_bool) == Some(true) {
                efforts.push(name.to_string());
            }
        }
        if !efforts.is_empty() {
            Some(efforts)
        } else if self.supported == Some(true) {
            Some(vec![
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
            ])
        } else {
            None
        }
    }
}

#[derive(Deserialize)]
struct AnthropicCapabilities {
    #[serde(default)]
    thinking: Option<CapabilityFlag>,
    #[serde(default)]
    supports_tools: Option<CapabilityFlag>,
    #[serde(default)]
    tools: Option<CapabilityFlag>,
    #[serde(default)]
    tool_use: Option<CapabilityFlag>,
    #[serde(default)]
    supports_attachments: Option<CapabilityFlag>,
    #[serde(default)]
    attachments: Option<CapabilityFlag>,
    #[serde(default)]
    vision: Option<CapabilityFlag>,
    #[serde(default)]
    image_input: Option<CapabilityFlag>,
    #[serde(default)]
    pdf_input: Option<CapabilityFlag>,
    #[serde(default)]
    supported_efforts: Option<Vec<String>>,
    #[serde(default)]
    effort: Option<AnthropicEffortCapabilities>,
}

/// Parse Anthropic `/v1/models` JSON payloads.
pub(crate) fn parse_anthropic_models_metadata(
    bytes: &[u8],
) -> Result<Vec<ApiModelMetadata>, ExecutorError> {
    #[derive(Deserialize)]
    struct Root {
        #[serde(default)]
        data: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        #[serde(default)]
        max_input_tokens: Option<u32>,
        #[serde(default)]
        context_window: Option<u32>,
        #[serde(default)]
        max_tokens: Option<u32>,
        #[serde(default)]
        max_output_tokens: Option<u32>,
        #[serde(default)]
        supported_efforts: Option<Vec<String>>,
        #[serde(default)]
        capabilities: Option<AnthropicCapabilities>,
    }

    let parsed: Root =
        serde_json::from_slice(bytes).map_err(|_| ExecutorError::UnexpectedResponse)?;
    Ok(parsed
        .data
        .into_iter()
        .filter(|e| !e.id.trim().is_empty())
        .map(|entry| {
            let caps = entry.capabilities.as_ref();
            let supports_thinking = caps
                .and_then(|c| c.thinking.as_ref())
                .and_then(CapabilityFlag::as_bool);
            let supports_tools = caps.and_then(|c| {
                c.supports_tools
                    .as_ref()
                    .and_then(CapabilityFlag::as_bool)
                    .or_else(|| c.tools.as_ref().and_then(CapabilityFlag::as_bool))
                    .or_else(|| c.tool_use.as_ref().and_then(CapabilityFlag::as_bool))
            });
            let supports_attachments = caps.and_then(|c| {
                let explicit = c
                    .supports_attachments
                    .as_ref()
                    .and_then(CapabilityFlag::as_bool)
                    .or_else(|| c.attachments.as_ref().and_then(CapabilityFlag::as_bool))
                    .or_else(|| c.vision.as_ref().and_then(CapabilityFlag::as_bool));
                if explicit.is_some() {
                    return explicit;
                }
                let img = c.image_input.as_ref().and_then(CapabilityFlag::as_bool);
                let pdf = c.pdf_input.as_ref().and_then(CapabilityFlag::as_bool);
                match (img, pdf) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (None, None) => None,
                }
            });
            let supported_efforts = entry
                .supported_efforts
                .or_else(|| caps.and_then(|c| c.supported_efforts.clone()))
                .or_else(|| {
                    caps.and_then(|c| {
                        c.effort
                            .as_ref()
                            .and_then(AnthropicEffortCapabilities::to_efforts)
                    })
                })
                .or_else(|| match supports_thinking {
                    Some(true) => Some(vec![
                        "low".to_string(),
                        "medium".to_string(),
                        "high".to_string(),
                    ]),
                    Some(false) => Some(Vec::new()),
                    None => None,
                });
            ApiModelMetadata {
                model_id: entry.id,
                context_window: entry.max_input_tokens.or(entry.context_window),
                max_output_tokens: entry.max_tokens.or(entry.max_output_tokens),
                supported_efforts,
                supports_thinking,
                supports_attachments,
                supports_tools,
            }
        })
        .collect())
}

/// Parse Gemini `/v1beta/models` JSON payloads.
pub(crate) fn parse_gemini_models_metadata(
    bytes: &[u8],
) -> Result<Vec<ApiModelMetadata>, ExecutorError> {
    #[derive(Deserialize)]
    struct Root {
        #[serde(default)]
        models: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        name: String,
        #[serde(rename = "inputTokenLimit", default)]
        input_token_limit: Option<u32>,
        #[serde(rename = "outputTokenLimit", default)]
        output_token_limit: Option<u32>,
        #[serde(default)]
        thinking: Option<bool>,
        #[serde(rename = "supportedGenerationMethods", default)]
        supported_generation_methods: Vec<String>,
    }

    let parsed: Root =
        serde_json::from_slice(bytes).map_err(|_| ExecutorError::UnexpectedResponse)?;
    Ok(parsed
        .models
        .into_iter()
        .filter_map(|entry| {
            let model_id = entry
                .name
                .strip_prefix("models/")
                .unwrap_or(&entry.name)
                .trim()
                .to_string();
            if model_id.is_empty() {
                return None;
            }
            let supports_generate = entry
                .supported_generation_methods
                .iter()
                .any(|m| m == "generateContent");
            let supported_efforts = if entry.thinking == Some(true) {
                Some(vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                ])
            } else if entry.thinking == Some(false) {
                Some(Vec::new())
            } else {
                None
            };
            Some(ApiModelMetadata {
                model_id,
                context_window: entry.input_token_limit,
                max_output_tokens: entry.output_token_limit,
                supported_efforts,
                supports_thinking: entry.thinking,
                supports_attachments: supports_generate.then_some(true),
                supports_tools: supports_generate.then_some(true),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_openrouter_models_metadata_zero_token() {
        let raw = br#"{
            "data": [
                {
                    "id": "gpt-5.6-terra",
                    "context_length": 400000,
                    "supported_parameters": ["tools", "reasoning_effort"],
                    "architecture": { "input_modalities": ["text", "image"] }
                },
                {
                    "id": "legacy-text",
                    "context_window": 16384,
                    "capabilities": { "thinking": false, "tools": false, "vision": false }
                }
            ]
        }"#;
        let items = parse_openai_models_metadata(raw).expect("parse openai metadata");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].model_id, "gpt-5.6-terra");
        assert_eq!(items[0].context_window, Some(400_000));
        assert_eq!(items[0].supports_thinking, Some(true));
        assert_eq!(items[0].supports_attachments, Some(true));
        assert_eq!(items[0].supports_tools, Some(true));
        assert_eq!(
            items[0].supported_efforts.as_deref(),
            Some(["low".to_string(), "medium".to_string(), "high".to_string()].as_slice())
        );

        assert_eq!(items[1].model_id, "legacy-text");
        assert_eq!(items[1].context_window, Some(16_384));
        assert_eq!(items[1].supports_thinking, Some(false));
        assert_eq!(items[1].supports_tools, Some(false));
        assert_eq!(items[1].supports_attachments, Some(false));
        assert_eq!(items[1].supported_efforts.as_deref(), Some(&[][..]));
    }

    #[test]
    fn parses_openrouter_mistral_opencode_zen_and_thin_openai_metadata() {
        let raw = br#"{
            "data": [
                {
                    "id": "openrouter/model-a",
                    "context_length": 200000,
                    "top_provider": { "context_length": 200000, "max_completion_tokens": 64000 },
                    "supported_parameters": ["tools", "reasoning"],
                    "architecture": { "input_modalities": ["text", "image"] }
                },
                {
                    "id": "mistral-large-latest",
                    "max_context_length": 131072,
                    "max_output_tokens": 16384,
                    "capabilities": { "function_calling": true, "vision": false }
                },
                {
                    "id": "opencode/zen-pro",
                    "limit": { "context": 262144, "output": 32768 },
                    "capabilities": {
                        "reasoning": true,
                        "toolcall": true,
                        "attachment": true,
                        "input": { "image": true, "pdf": true }
                    }
                },
                {
                    "id": "gpt-4o"
                }
            ]
        }"#;
        let items = parse_openai_models_metadata(raw).expect("parse extended openai metadata");
        assert_eq!(items.len(), 4);

        // OpenRouter top_provider.max_completion_tokens
        assert_eq!(items[0].model_id, "openrouter/model-a");
        assert_eq!(items[0].context_window, Some(200_000));
        assert_eq!(items[0].max_output_tokens, Some(64_000));
        assert_eq!(items[0].supports_tools, Some(true));
        assert_eq!(items[0].supports_thinking, Some(true));
        assert_eq!(items[0].supports_attachments, Some(true));

        // Mistral max_context_length & capabilities.function_calling
        assert_eq!(items[1].model_id, "mistral-large-latest");
        assert_eq!(items[1].context_window, Some(131_072));
        assert_eq!(items[1].max_output_tokens, Some(16_384));
        assert_eq!(items[1].supports_tools, Some(true));
        assert_eq!(items[1].supports_attachments, Some(false));

        // OpenCode Zen limit.context / limit.output / capabilities
        assert_eq!(items[2].model_id, "opencode/zen-pro");
        assert_eq!(items[2].context_window, Some(262_144));
        assert_eq!(items[2].max_output_tokens, Some(32_768));
        assert_eq!(items[2].supports_thinking, Some(true));
        assert_eq!(items[2].supports_tools, Some(true));
        assert_eq!(items[2].supports_attachments, Some(true));

        // Thin /v1/models entry supplies no capability fields.
        assert_eq!(items[3].model_id, "gpt-4o");
        assert!(!items[3].has_any_field());
    }

    #[test]
    fn parses_anthropic_and_gemini_models_metadata_zero_token() {
        let anthropic_raw = br#"{
            "data": [
                {
                    "id": "claude-sonnet-4-20250514",
                    "max_input_tokens": 200000,
                    "max_tokens": 64000,
                    "capabilities": {
                        "thinking": { "supported": true },
                        "tool_use": { "supported": true },
                        "image_input": { "supported": true },
                        "effort": {
                            "supported": true,
                            "low": { "supported": true },
                            "medium": { "supported": true },
                            "high": { "supported": true },
                            "max": { "supported": true }
                        }
                    }
                },
                {
                    "id": "claude-legacy-text",
                    "max_input_tokens": 100000,
                    "max_tokens": 8192,
                    "capabilities": {
                        "thinking": false,
                        "supports_tools": false,
                        "supports_attachments": false
                    }
                }
            ]
        }"#;
        let a_items = parse_anthropic_models_metadata(anthropic_raw).expect("parse anthropic");
        assert_eq!(a_items.len(), 2);
        assert_eq!(a_items[0].model_id, "claude-sonnet-4-20250514");
        assert_eq!(a_items[0].context_window, Some(200_000));
        assert_eq!(a_items[0].max_output_tokens, Some(64_000));
        assert_eq!(a_items[0].supports_thinking, Some(true));
        assert_eq!(a_items[0].supports_tools, Some(true));
        assert_eq!(a_items[0].supports_attachments, Some(true));
        assert_eq!(
            a_items[0].supported_efforts.as_deref(),
            Some(
                [
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                    "xhigh".to_string()
                ]
                .as_slice()
            )
        );
        assert_eq!(a_items[1].model_id, "claude-legacy-text");
        assert_eq!(a_items[1].max_output_tokens, Some(8_192));
        assert_eq!(a_items[1].supports_thinking, Some(false));
        assert_eq!(a_items[1].supports_tools, Some(false));
        assert_eq!(a_items[1].supports_attachments, Some(false));
        assert_eq!(a_items[1].supported_efforts.as_deref(), Some(&[][..]));

        let gemini_raw = br#"{
            "models": [
                {
                    "name": "models/gemini-2.5-pro",
                    "inputTokenLimit": 1048576,
                    "outputTokenLimit": 65536,
                    "thinking": true,
                    "supportedGenerationMethods": ["generateContent"]
                },
                {
                    "name": "models/gemini-2.5-flash-lite",
                    "inputTokenLimit": 1048576,
                    "outputTokenLimit": 65536,
                    "thinking": false,
                    "supportedGenerationMethods": ["generateContent"]
                }
            ]
        }"#;
        let g_items = parse_gemini_models_metadata(gemini_raw).expect("parse gemini");
        assert_eq!(g_items.len(), 2);
        assert_eq!(g_items[0].model_id, "gemini-2.5-pro");
        assert_eq!(g_items[0].context_window, Some(1_048_576));
        assert_eq!(g_items[0].max_output_tokens, Some(65_536));
        assert_eq!(g_items[0].supports_thinking, Some(true));
        assert_eq!(g_items[1].model_id, "gemini-2.5-flash-lite");
        assert_eq!(g_items[1].max_output_tokens, Some(65_536));
        assert_eq!(g_items[1].supports_thinking, Some(false));
        assert_eq!(g_items[1].supported_efforts.as_deref(), Some(&[][..]));
    }

    #[test]
    fn openai_models_url_maps_all_preset_endpoints() {
        for preset in super::super::catalog::openai::OPENAI_COMPAT_PRESETS {
            assert!(
                preset.endpoint.starts_with("https://"),
                "preset {} endpoint must use https",
                preset.name
            );
            let models_url = openai_models_url(preset.endpoint);
            assert!(
                models_url.starts_with("https://") && models_url.ends_with("/models"),
                "preset {} models url invalid: {models_url}",
                preset.name
            );
        }
    }
}
