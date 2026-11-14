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
use super::transport::{Credential, HttpClient, PostOutcome};

const METADATA_TIMEOUT: Duration = Duration::from_secs(10);

/// Capability fields discovered from a provider's zero-token `/models` endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ApiModelMetadata {
    /// Upstream model identifier.
    pub model_id: String,
    /// Reported input/context window in tokens, when exposed by the API.
    pub context_window: Option<u32>,
    /// Supported reasoning efforts inferred from API metadata parameters, if any.
    pub supported_efforts: Option<Vec<String>>,
    /// Whether the API metadata explicitly advertises thinking/reasoning support.
    pub supports_thinking: Option<bool>,
    /// Whether the API metadata explicitly advertises image/file/multimodal input.
    pub supports_attachments: Option<bool>,
    /// Whether the API metadata explicitly advertises tool/function calling.
    pub supports_tools: Option<bool>,
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

#[derive(Deserialize)]
struct OpenAiModelsRoot {
    #[serde(default)]
    data: Vec<OpenAiModelEntry>,
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
    vision: Option<bool>,
    #[serde(default)]
    attachments: Option<bool>,
}

fn map_openai_entry(entry: OpenAiModelEntry) -> ApiModelMetadata {
    let context_window = entry
        .context_length
        .or(entry.context_window)
        .or(entry.max_context_len);
    let params_lower: Vec<String> = entry
        .supported_parameters
        .iter()
        .map(|p| p.to_ascii_lowercase())
        .collect();
    let supports_tools = entry
        .capabilities
        .as_ref()
        .and_then(|c| c.tools)
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
        .and_then(|c| c.attachments.or(c.vision))
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
    let supported_efforts = entry.supported_efforts.or_else(|| {
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
        } else {
            None
        }
    });
    ApiModelMetadata {
        model_id: entry.id,
        context_window,
        supported_efforts,
        supports_thinking,
        supports_attachments,
        supports_tools,
    }
}

/// Parse `OpenAI` / `OpenRouter` / vLLM / compat `/v1/models` JSON payloads.
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
    }

    let parsed: Root =
        serde_json::from_slice(bytes).map_err(|_| ExecutorError::UnexpectedResponse)?;
    Ok(parsed
        .data
        .into_iter()
        .filter(|e| !e.id.trim().is_empty())
        .map(|entry| ApiModelMetadata {
            model_id: entry.id,
            context_window: entry.max_input_tokens.or(entry.context_window),
            supported_efforts: None,
            supports_thinking: None,
            supports_attachments: None,
            supports_tools: None,
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
    }

    #[test]
    fn parses_anthropic_and_gemini_models_metadata_zero_token() {
        let anthropic_raw = br#"{
            "data": [{ "id": "claude-sonnet-5", "max_input_tokens": 200000 }]
        }"#;
        let a_items = parse_anthropic_models_metadata(anthropic_raw).expect("parse anthropic");
        assert_eq!(a_items.len(), 1);
        assert_eq!(a_items[0].model_id, "claude-sonnet-5");
        assert_eq!(a_items[0].context_window, Some(200_000));

        let gemini_raw = br#"{
            "models": [
                {
                    "name": "models/gemini-3.1-pro-preview",
                    "inputTokenLimit": 1048576,
                    "thinking": true,
                    "supportedGenerationMethods": ["generateContent"]
                },
                {
                    "name": "models/gemini-2.5-flash-lite",
                    "inputTokenLimit": 1000000,
                    "thinking": false,
                    "supportedGenerationMethods": ["generateContent"]
                }
            ]
        }"#;
        let g_items = parse_gemini_models_metadata(gemini_raw).expect("parse gemini");
        assert_eq!(g_items.len(), 2);
        assert_eq!(g_items[0].model_id, "gemini-3.1-pro-preview");
        assert_eq!(g_items[0].context_window, Some(1_048_576));
        assert_eq!(g_items[0].supports_thinking, Some(true));
        assert_eq!(g_items[1].model_id, "gemini-2.5-flash-lite");
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
