//! OpenAI provider integration: a concrete [`ProviderExecutor`] (ROADMAP.md
//! Phase 3 вЂ” AI Providers; ARCHITECTURE.md В§7).
//!
//! This is the first concrete provider implementation behind the
//! provider-independent execution boundary. It translates a provider-agnostic
//! [`AiRequest`] into a non-streaming OpenAI Chat Completions request, sends it
//! over HTTPS using the crate's HTTP facility (`reqwest`), and normalizes the
//! response into the provider-independent [`AiResponse`].
//!
//! Isolation: every OpenAI-specific wire type lives in this module. Nothing
//! OpenAI-specific is exposed through [`application::execution`]; the boundary
//! only ever sees [`AiRequest`]/[`AiResponse`]/[`ExecutorError`]. The same
//! boundary can later host Anthropic, Gemini, DeepSeek, Kimi, or Grok without
//! touching this contract.
//!
//! # Security (ARCHITECTURE.md В§9, В§11, В§12)
//!
//! - The credential is supplied by the caller from the [`CredentialStore`] for
//!   the duration of the call only; it is never persisted, logged, or returned.
//! - The credential is sent only in the `Authorization` header via
//!   `reqwest`'s `bearer_auth`, never in the body.
//! - Failed responses are read and classified by HTTP status **plus the
//!   `Retry-After` header and common JSON body shapes**; only the failure
//!   *category* ever reaches an error or log — never the credential, the
//!   request payload, or raw body text.
//! - All failures collapse to the single provider-independent
//!   [`ExecutorError::Failure`]; the internal [`OpenAiError`] classification
//!   (authentication, invalid request, provider/network, unexpected response)
//!   is recorded in the logs by category only.

// The module docs deliberately reference product/brand names (OpenAI,
// Anthropic, Gemini, DeepSeek, Kimi, Grok, Chat Completions), which the
// `doc_markdown` pedantic lint flags as needing backticks. Allow it locally.
#![allow(clippy::doc_markdown)]

use crate::application::agent::control::CancellationToken;
use crate::application::execution::{
    model_info_for, AiAttachmentPayload, AiMessage, AiRequest, AiResponse, AiRole, ExecutorError,
    ModelConfig, ProviderExecutor,
};
// `AiAttachment` is referenced only by this module's unit tests.
#[cfg(test)]
use crate::application::execution::AiAttachment;

use super::transport::{Credential, HttpClient, PostOutcome, PostRequest};
use serde::{Deserialize, Serialize};

/// OpenAI's Chat Completions endpoint.
const ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

/// Internal provider name (DATABASE.md В§7.5); the keyring namespace key.
pub(crate) const PROVIDER_NAME: &str = "openai";
/// User-facing provider label.
pub(crate) const PROVIDER_DISPLAY_NAME: &str = "OpenAI";

/// OpenAI models currently supported by the provider (DATABASE.md В§7.5: model
/// lists are hardcoded in the MVP and managed by the application layer).
///
/// The selected model is passed through unchanged and is never validated
/// against this list at runtime (never silently substituted, never rejected):
/// this set documents the currently supported models and anchors the
/// model-selection surface so the UI can present only supported choices.
///
/// Pricing is governed by the policy table in
/// `crate::application::agent::pricing` (DATABASE.md В§7.8); the first entry
/// is the provider default consumed as `models[0]` by the selection surface.
pub(crate) const SUPPORTED_MODELS: &[&str] = &[
    // Default: best balance of cost and capability.
    "gpt-5.6-terra",
    // Fast/cheap tier.
    "gpt-5.6-luna",
    // Best-quality flagship tier.
    "gpt-5.6-sol",
];
/// OpenAI-compatible provider identities sharing [`OpenAiExecutor`].
///
/// Curated hardcoded model shortlists (DATABASE.md §7.5); `MODELS[0]` is the
/// provider default consumed by the selection surface. Pricing inherits the
/// shared policy rate.
///
/// Keep rule (smoke-gated, 2026-09-12): an ID stays listed iff a live POST to
/// the provider's `chat/completions` endpoint returns chat 2xx for it; an ID
/// is agent-usable iff the tools leg returns 2xx. IDs that return 429 on both
/// legs stay listed (rate-limited, not dead) only when explicitly noted;
/// anything failing the chat leg is dropped, never re-added from the catalog.
///
/// 1.2.3 (2026-09-13): OpenRouter list re-gated — dropped `minimax-m3:free`,
/// `minimax-m2.7:free`, `glm-5.2:free` (chat 404) and `ultra-550b-a55b:free`
/// (no HTTP response twice); added four live-proven chat+tools 200 IDs.
/// xKiro list untouched (all 8 chat 404, no live replacements proven).
pub(crate) const XKIRO_NAME: &str = "xkiro";
pub(crate) const XKIRO_DISPLAY_NAME: &str = "xKiro";
pub(crate) const XKIRO_ENDPOINT: &str = "https://api.xkiro.com/v1/chat/completions";
pub(crate) const XKIRO_MODELS: &[&str] = &[
    "deepseek/deepseek-v4-flash",
    "qwen/qwen3.5-omni-plus:free",
    "minimax/minimax-m3:free",
    "minimax/minimax-m2.7:free",
    "qwen/qwen3.5-plus:free",
    "qwen/qwen3.6-plus:free",
    "qwen/qwen3.7-plus:free",
    "deepseek/deepseek-v4-pro",
];
pub(crate) const OPENROUTER_NAME: &str = "openrouter";
pub(crate) const OPENROUTER_DISPLAY_NAME: &str = "OpenRouter";
pub(crate) const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
pub(crate) const OPENROUTER_MODELS: &[&str] = &[
    "inclusionai/ling-3.0-flash-fin:free",
    "nvidia/nemotron-3.5-lightning:free",
    "nvidia/nemotron-3-super-120b-a12b:free",
    "cohere/north-mini-code:free",
    "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
    "inclusionai/ling-3.0-flash-sante:free",
    "inclusionai/ling-3.0-flash-vl:free",
    "liquid/lfm-2.5-2.6b:free",
];
pub(crate) const NVIDIA_NAME: &str = "nvidia";
pub(crate) const NVIDIA_DISPLAY_NAME: &str = "NVIDIA NIM";
pub(crate) const NVIDIA_ENDPOINT: &str = "https://integrate.api.nvidia.com/v1/chat/completions";
pub(crate) const NVIDIA_MODELS: &[&str] = &[
    "nvidia/nemotron-3-super-120b-a12b",
    "nvidia/nemotron-3-ultra-550b-a55b",
    "meta/llama-3.2-11b-vision-instruct",
    "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning",
    "openai/gpt-oss-20b",
];
pub(crate) const OPENCODE_ZEN_NAME: &str = "opencode_zen";
pub(crate) const OPENCODE_ZEN_DISPLAY_NAME: &str = "OpenCode Zen";
pub(crate) const OPENCODE_ZEN_ENDPOINT: &str = "https://opencode.ai/zen/v1/chat/completions";
pub(crate) const OPENCODE_ZEN_MODELS: &[&str] = &[
    "ling-3.0-flash-fin-free",
    "nemotron-3-ultra-free",
    "nemotron-3.5-lightning-free",
    "big-pickle",
    "mimo-v2.5-free",
];
/// User-configured OpenAI-compatible endpoint identity (DATABASE.md §7.5).
///
/// Unlike the curated providers above, this entry carries no hardcoded model
/// shortlist: the base URL, model, organization, and extra headers come from
/// the user's own configuration (persisted as plain settings), while the API
/// key lives only in the OS keyring under this name (ARCHITECTURE.md §12).
/// The endpoint speaks the OpenAI Chat Completions wire shape through the
/// shared [`OpenAiExecutor`].
pub(crate) const COMPAT_NAME: &str = "openai_compat";
/// User-facing label for the configurable endpoint.
pub(crate) const COMPAT_DISPLAY_NAME: &str = "OpenAI Compatible";
///
/// Maximum accepted base-URL length (characters): generous for paths and
/// ports, bounded against runaway values.
const MAX_BASE_URL_LEN: usize = 2_048;
/// Maximum accepted model identifier length (characters).
const MAX_MODEL_LEN: usize = 256;
/// Maximum accepted organization identifier length (characters).
const MAX_ORGANIZATION_LEN: usize = 128;
/// Maximum number of user-configured extra headers.
const MAX_EXTRA_HEADERS: usize = 16;
/// Maximum accepted header name/value lengths (characters).
const MAX_HEADER_NAME_LEN: usize = 64;
const MAX_HEADER_VALUE_LEN: usize = 2_048;

/// User configuration for the generic OpenAI-compatible endpoint.
///
/// Deliberately carries **no secret**: the API key is keyed by [`COMPAT_NAME`]
/// in the OS keyring and travels only as the call-scoped `credential`
/// argument. Every field here is non-sensitive endpoint metadata safe to
/// persist as plain settings and to surface to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompatConfig {
    /// Fully-qualified Chat Completions URL, e.g.
    /// `https://proxy.example.com/v1/chat/completions`.
    pub base_url: String,
    /// Model identifier sent verbatim as the request `model`.
    pub model: String,
    /// Optional organization identifier, sent as `OpenAI-Organization`.
    #[serde(default)]
    pub organization: Option<String>,
    /// Optional extra request headers (`name`, `value` pairs).
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Whether the endpoint accepts function-calling tools. When `false`,
    /// tools are never serialized (graceful degrade via capabilities).
    #[serde(default = "default_compat_tools_supported")]
    pub supports_tools: bool,
}

/// Default for [`CompatConfig::supports_tools`]: most OpenAI-compatible
/// endpoints accept function calling; the user toggles it off for endpoints
/// that reject the `tools` member.
fn default_compat_tools_supported() -> bool {
    true
}

impl Default for CompatConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            organization: None,
            headers: Vec::new(),
            supports_tools: default_compat_tools_supported(),
        }
    }
}

/// Field-level presence of a [`CompatConfig`] for the UI.
///
/// Presence booleans only — no secret material exists on this path by
/// construction (the key lives in the keyring and is reported separately via
/// `has_provider_credential`). `base_url_valid` is the [`CompatConfig::validate`]
/// URL verdict so the UI can prompt before a request is ever attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct CompatFieldPresence {
    /// A base URL value is configured (non-blank).
    pub has_base_url: bool,
    /// The configured base URL passes validation.
    pub base_url_valid: bool,
    /// A model identifier is configured (non-blank).
    pub has_model: bool,
    /// An organization identifier is configured.
    pub has_organization: bool,
    /// Number of configured extra headers.
    pub header_count: usize,
}

/// Secret-free validation failures for a [`CompatConfig`].
///
/// Category-only: variants never echo the URL (which may embed credentials),
/// header names/values (which may carry alternate secrets), or any key
/// material — only the offending *category* and safe limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompatConfigError {
    /// No base URL is configured.
    MissingBaseUrl,
    /// The base URL does not use the `http` or `https` scheme.
    UnsupportedScheme,
    /// The base URL has no host to connect to.
    MissingHost,
    /// The base URL embeds `user:password@` credentials.
    CredentialsInUrl,
    /// The base URL carries an unparsable port.
    InvalidPort,
    /// The base URL contains control characters or ASCII whitespace.
    InvalidCharacters,
    /// The base URL exceeds [`MAX_BASE_URL_LEN`].
    BaseUrlTooLong,
    /// No model identifier is configured.
    MissingModel,
    /// The model identifier exceeds [`MAX_MODEL_LEN`].
    ModelTooLong,
    /// The organization identifier is malformed.
    InvalidOrganization,
    /// More than [`MAX_EXTRA_HEADERS`] extra headers are configured.
    TooManyHeaders,
    /// An extra header name is malformed or reserved.
    InvalidHeaderName,
    /// An extra header value is malformed (e.g. carries CR/LF).
    InvalidHeaderValue,
}

impl std::fmt::Display for CompatConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBaseUrl => write!(f, "no base URL is configured"),
            Self::UnsupportedScheme => {
                write!(f, "the base URL must use the http or https scheme")
            }
            Self::MissingHost => write!(f, "the base URL has no host"),
            Self::CredentialsInUrl => write!(
                f,
                "the base URL must not embed credentials; the API key belongs in the OS keyring"
            ),
            Self::InvalidPort => write!(f, "the base URL carries an invalid port"),
            Self::InvalidCharacters => {
                write!(f, "the base URL contains control characters or whitespace")
            }
            Self::BaseUrlTooLong => {
                write!(f, "the base URL exceeds {MAX_BASE_URL_LEN} characters")
            }
            Self::MissingModel => write!(f, "no model is configured"),
            Self::ModelTooLong => write!(f, "the model exceeds {MAX_MODEL_LEN} characters"),
            Self::InvalidOrganization => write!(f, "the organization identifier is invalid"),
            Self::TooManyHeaders => {
                write!(
                    f,
                    "at most {MAX_EXTRA_HEADERS} extra headers may be configured"
                )
            }
            Self::InvalidHeaderName => write!(f, "an extra header name is invalid or reserved"),
            Self::InvalidHeaderValue => write!(f, "an extra header value is invalid"),
        }
    }
}

impl std::error::Error for CompatConfigError {}

impl CompatConfig {
    /// Validate every field, returning the first failure category.
    ///
    /// Pure and secret-free: the returned [`CompatConfigError`] never echoes
    /// the URL, header names/values, or any key material — only the failure
    /// category and safe limits.
    pub(crate) fn validate(&self) -> Result<(), CompatConfigError> {
        validate_base_url(&self.base_url)?;
        validate_model(&self.model)?;
        if let Some(organization) = self.organization.as_deref() {
            validate_organization(organization)?;
        }
        validate_headers(&self.headers)?;
        Ok(())
    }

    /// The endpoint URL to send to: the trimmed base URL.
    ///
    /// Callers must [`Self::validate`] first; the executor re-validates and
    /// fails closed, so an unvalidated config can never send to a surprising
    /// target.
    pub(crate) fn normalized_endpoint(&self) -> String {
        self.base_url.trim().to_string()
    }

    /// Project the field-level [`CompatFieldPresence`] for the UI.
    ///
    /// Presence booleans only; credential presence is composed separately by
    /// the application layer from the keyring.
    #[must_use]
    pub(crate) fn field_presence(&self) -> CompatFieldPresence {
        let base_url = self.base_url.trim();
        CompatFieldPresence {
            has_base_url: !base_url.is_empty(),
            base_url_valid: validate_base_url(&self.base_url).is_ok(),
            has_model: !self.model.trim().is_empty(),
            has_organization: self
                .organization
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            header_count: self.headers.len(),
        }
    }
}

/// Validate a candidate base URL without fetching it (local-first).
///
/// Accepts `http`/`https` URLs with a non-empty host, an optional numeric
/// port, and any path/query. Rejects embedded `user:password@` credentials
/// (the key belongs in the keyring), control characters, and ASCII
/// whitespace. Category-only failures: the URL itself is never echoed.
fn validate_base_url(url: &str) -> Result<(), CompatConfigError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(CompatConfigError::MissingBaseUrl);
    }
    if trimmed.len() > MAX_BASE_URL_LEN {
        return Err(CompatConfigError::BaseUrlTooLong);
    }
    if trimmed
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(CompatConfigError::InvalidCharacters);
    }
    // Scheme split without allocating a lowered copy of the whole URL.
    let Some(after_scheme) = trimmed.split_once("://").map(|(_, rest)| rest) else {
        return Err(CompatConfigError::UnsupportedScheme);
    };
    let scheme_len = trimmed.len() - after_scheme.len() - "://".len();
    if !trimmed[..scheme_len].eq_ignore_ascii_case("http")
        && !trimmed[..scheme_len].eq_ignore_ascii_case("https")
    {
        return Err(CompatConfigError::UnsupportedScheme);
    }
    // Authority runs to the first path/query/fragment separator.
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() {
        return Err(CompatConfigError::MissingHost);
    }
    // Embedded credentials (`user@`, `user:pass@`) belong in the keyring,
    // never in the stored URL — and must never be echoed back.
    if authority.contains('@') {
        return Err(CompatConfigError::CredentialsInUrl);
    }
    // Optional numeric port after the host (`[v6]` hosts keep their brackets).
    let host_port = authority;
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        let Some(end) = bracketed.find(']') else {
            return Err(CompatConfigError::MissingHost);
        };
        let zone = &bracketed[..end];
        if zone.is_empty() {
            return Err(CompatConfigError::MissingHost);
        }
        let rest = &bracketed[end + 1..];
        if !rest.is_empty() {
            let Some(port) = rest.strip_prefix(':') else {
                return Err(CompatConfigError::InvalidPort);
            };
            validate_port(port)?;
        }
        zone
    } else if let Some((host, port)) = host_port.rsplit_once(':') {
        // A bare colon introduces a port only when the tail is all digits;
        // anything else (e.g. a second IPv6 group without brackets) has no
        // host to connect to.
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(CompatConfigError::MissingHost);
        }
        validate_port(port)?;
        host
    } else {
        host_port
    };
    if host.is_empty() {
        return Err(CompatConfigError::MissingHost);
    }
    Ok(())
}

/// Validate a numeric port: 1–65535 with no leading-zero or empty games.
fn validate_port(port: &str) -> Result<(), CompatConfigError> {
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CompatConfigError::InvalidPort);
    }
    match port.parse::<u32>() {
        Ok(number) if (1..=65_535).contains(&number) => Ok(()),
        _ => Err(CompatConfigError::InvalidPort),
    }
}

/// Validate the model identifier: non-blank, bounded, single-line.
fn validate_model(model: &str) -> Result<(), CompatConfigError> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err(CompatConfigError::MissingModel);
    }
    if trimmed.len() > MAX_MODEL_LEN {
        return Err(CompatConfigError::ModelTooLong);
    }
    if trimmed
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(CompatConfigError::MissingModel);
    }
    Ok(())
}

/// Validate the organization identifier: visible ASCII, no whitespace.
fn validate_organization(organization: &str) -> Result<(), CompatConfigError> {
    let trimmed = organization.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_ORGANIZATION_LEN
        || !trimmed.bytes().all(|byte| (0x21..=0x7E).contains(&byte))
    {
        return Err(CompatConfigError::InvalidOrganization);
    }
    Ok(())
}

/// Header names the transport already owns: user configuration must not
/// shadow authentication, framing, or the organization header.
fn is_reserved_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("content-type")
        || name.eq_ignore_ascii_case("content-length")
        || name.eq_ignore_ascii_case("openai-organization")
}

/// A header name is an HTTP token (`RFC 9110 §5.1`) within bounds.
fn is_header_token(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_HEADER_NAME_LEN
        && name.bytes().all(|byte| {
            matches!(byte,
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'
            | b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+'
            | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
        })
}

/// Validate user-configured extra headers.
///
/// Failures are category-only: header values may carry alternate secrets, so
/// neither names nor values are ever echoed.
fn validate_headers(headers: &[(String, String)]) -> Result<(), CompatConfigError> {
    if headers.len() > MAX_EXTRA_HEADERS {
        return Err(CompatConfigError::TooManyHeaders);
    }
    for (name, value) in headers {
        if !is_header_token(name.trim()) || is_reserved_header(name.trim()) {
            return Err(CompatConfigError::InvalidHeaderName);
        }
        let trimmed = value.trim();
        if trimmed.is_empty()
            || trimmed.len() > MAX_HEADER_VALUE_LEN
            || trimmed.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(CompatConfigError::InvalidHeaderValue);
        }
    }
    Ok(())
}

/// Stateless over the shared cancellable transport client so connections
/// are pooled across requests; the per-request credential and request payload
/// are passed into each [`ProviderExecutor::execute`] call and dropped on
/// return.
pub(crate) struct OpenAiExecutor {
    client: HttpClient,
    endpoint: String,
    name: &'static str,
    extra_headers: Vec<(&'static str, &'static str)>,
    /// Runtime-configured extra headers (user-configured endpoint headers).
    owned_headers: Vec<(String, String)>,
    /// Optional organization identifier, sent as `OpenAI-Organization`.
    organization: Option<String>,
    /// Endpoint tool-calling support override. `None` derives the verdict
    /// from the curated [`model_info_for`] projection; `Some` (the
    /// user-configured endpoint) is honored verbatim so an endpoint without
    /// function-calling support never receives an incompatible `tools` member.
    tools_supported: Option<bool>,
}

impl OpenAiExecutor {
    /// Create an executor targeting the OpenAI production endpoint.
    pub(crate) fn new() -> Self {
        Self::with_endpoint(ENDPOINT.to_string())
    }

    /// Create an executor targeting an explicit `endpoint` (used by tests,
    /// including the command-layer threading regression test, to exercise
    /// the full request/response path without a live OpenAI service).
    pub(crate) fn with_endpoint(endpoint: String) -> Self {
        Self::compatible(PROVIDER_NAME, endpoint)
    }

    /// Create an OpenAI-compatible executor with a distinct provider identity
    /// at `endpoint` (no extra headers).
    pub(crate) fn compatible(name: &'static str, endpoint: String) -> Self {
        Self {
            client: HttpClient::new(),
            endpoint,
            name,
            extra_headers: Vec::new(),
            owned_headers: Vec::new(),
            organization: None,
            tools_supported: None,
        }
    }

    /// Create an OpenAI-compatible executor with static extra headers applied
    /// after `bearer_auth` (OpenRouter Referer/Title only).
    pub(crate) fn compatible_with_headers(
        name: &'static str,
        endpoint: String,
        headers: &[(&'static str, &'static str)],
    ) -> Self {
        Self {
            client: HttpClient::new(),
            endpoint,
            name,
            extra_headers: headers.to_vec(),
            owned_headers: Vec::new(),
            organization: None,
            tools_supported: None,
        }
    }

    /// Create an executor for the user-configured OpenAI-compatible endpoint.
    ///
    /// The caller must [`CompatConfig::validate`] first; the stored endpoint
    /// is re-validated on every [`Self::run`] and fails closed, so an
    /// unvalidated config can never send to a surprising target. The API key
    /// is not part of the config — it arrives per call as `credential` from
    /// the keyring.
    pub(crate) fn compatible_with_config(name: &'static str, config: &CompatConfig) -> Self {
        Self {
            client: HttpClient::new(),
            endpoint: config.normalized_endpoint(),
            name,
            extra_headers: Vec::new(),
            owned_headers: config.headers.clone(),
            organization: config.organization.clone(),
            tools_supported: Some(config.supports_tools),
        }
    }

    /// Translate, send, and normalize one request.
    ///
    /// Returns a provider-independent [`AiResponse`] on success, or a
    /// classified [`OpenAiError`] describing the failure category.
    fn run(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
    ) -> Result<AiResponse, OpenAiError> {
        // Fail closed on a misconfigured endpoint before any network
        // activity: the error is category-only (never the URL or credential).
        if validate_base_url(&self.endpoint).is_err() {
            log::warn!("{name} request failed: invalid endpoint", name = self.name);
            return Err(OpenAiError::InvalidRequest);
        }
        // Capability-gated tool calling: an endpoint without function-calling
        // support never receives the `tools` member (graceful degrade). The
        // user-configured endpoint honors its own toggle; curated providers
        // derive the verdict from the local capability projection.
        let tools_supported = self
            .tools_supported
            .unwrap_or_else(|| model_info_for(self.name, &request.model).supports_tools);
        let body = chat_completion_request(request, tools_supported);
        let wire = serde_json::to_vec(&body).map_err(|_| OpenAiError::UnexpectedResponse)?;
        // The organization rides its own header value, validated upstream.
        let mut owned_headers = self.owned_headers.clone();
        if let Some(organization) = self.organization.as_deref() {
            owned_headers.push(("OpenAI-Organization".to_string(), organization.to_string()));
        }
        let outcome = self
            .client
            .post(
                token,
                &PostRequest {
                    url: self.endpoint.clone(),
                    credential: Credential::Bearer(credential),
                    extra_headers: &self.extra_headers,
                    extra_headers_owned: &owned_headers,
                    body: &wire,
                    timeout: request.request_timeout,
                },
            )
            .map_err(|err| {
                if matches!(err, ExecutorError::Cancelled) {
                    OpenAiError::Cancelled
                } else {
                    // The shared transport only ever reports `Network` besides
                    // cancellation; anything else would be a contract break,
                    // surfaced here as a transport failure rather than a
                    // success.
                    OpenAiError::Network
                }
            })?;
        match outcome {
            PostOutcome::Success(bytes) => {
                let response: ChatCompletionResponse =
                    serde_json::from_slice(&bytes).map_err(|_| OpenAiError::UnexpectedResponse)?;
                to_ai_response(response)
            }
            PostOutcome::Failure(snapshot) => {
                if snapshot.is_context_length_exceeded {
                    Err(OpenAiError::ContextLengthExceeded)
                } else {
                    Err(classify_status(snapshot.status, snapshot.retry_after_secs))
                }
            }
        }
    }
}

impl ProviderExecutor for OpenAiExecutor {
    fn execute(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
    ) -> Result<AiResponse, ExecutorError> {
        match self.run(request, credential, token) {
            Ok(response) => Ok(response),
            Err(error) => {
                // Record only the classification category; never the credential
                // or request payload (ARCHITECTURE.md В§9, В§11).
                log::warn!("{name} request failed: {error}", name = self.name);
                Err(match error {
                    OpenAiError::InvalidRequest => ExecutorError::InvalidRequest,
                    OpenAiError::ContextLengthExceeded => ExecutorError::ContextLengthExceeded,
                    OpenAiError::Authentication => ExecutorError::Authentication,
                    OpenAiError::Network => ExecutorError::Network,
                    OpenAiError::PaymentRequired => ExecutorError::PaymentRequired,
                    OpenAiError::RateLimited { retry_after_secs } => {
                        ExecutorError::RateLimited { retry_after_secs }
                    }
                    OpenAiError::ProviderUnavailable => ExecutorError::ProviderUnavailable,
                    OpenAiError::UnexpectedResponse => ExecutorError::UnexpectedResponse,
                    OpenAiError::Provider => ExecutorError::Failure,
                    OpenAiError::Cancelled => ExecutorError::Cancelled,
                })
            }
        }
    }
}
/// OpenAI request body mapped from the provider-independent [`AiRequest`].
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tools: Vec<OpenAiWireTool>,
    /// Sampling temperature from [`ModelConfig`], sent only when the endpoint
    /// supports the field and the value is in range (`0.0..=2.0`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    temperature: Option<f32>,
    /// Output-token cap from [`ModelConfig`], sent only when positive.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct OpenAiWireTool {
    r#type: &'static str,
    function: OpenAiWireFunction,
}

#[derive(Debug, Serialize)]
struct OpenAiWireFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// One OpenAI chat message.
///
/// `content` is serialized as `null` for assistant turns that carry only
/// `tool_calls` (OpenAI contract); regular turns keep the plain-string or
/// parts form of the pre-FR-008 / FR-008 shapes. Tool-result turns set
/// `tool_call_id` and a text content.
#[derive(Debug, Serialize)]
struct OpenAiMessage {
    role: String,
    // Serialized explicitly (None renders as JSON null) so assistant
    // tool-call turns carry `"content": null` per the OpenAI contract.
    content: Option<OpenAiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiReqWireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

/// OpenAI assistant `tool_calls` entry (request direction).
#[derive(Debug, Serialize)]
struct OpenAiReqWireToolCall {
    id: String,
    r#type: &'static str,
    function: OpenAiReqWireFunctionCall,
}

/// OpenAI assistant `function` payload: `name` plus the raw `arguments`
/// string exactly as the model produced it.
#[derive(Debug, Serialize)]
struct OpenAiReqWireFunctionCall {
    name: String,
    arguments: String,
}

/// OpenAI Chat Completions `content` values: a plain string, or an array of
/// text/image parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}
/// One OpenAI content part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiContentPart {
    /// A plain text segment.
    Text { text: String },
    /// An image supplied as a base64 data URI (OpenAI has no inline PDF or
    /// arbitrary-binary input in Chat Completions).
    ImageUrl { image_url: OpenAiImageUrl },
}

/// The data-URI wrapper of an OpenAI image part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct OpenAiImageUrl {
    url: String,
}

/// Translate a provider-independent request into an OpenAI request body.
///
/// `tools_supported` is the capability verdict for this endpoint: when
/// `false`, the `tools` member is omitted entirely (graceful degrade).
///
/// The selected model is passed through unchanged вЂ” it is never silently
/// substituted (FR-004). System, user, and assistant messages map to the
/// corresponding OpenAI `role`.
fn chat_completion_request(request: &AiRequest, tools_supported: bool) -> ChatCompletionRequest {
    let (temperature, max_tokens) = sampling_params(request.model_config.as_ref());
    ChatCompletionRequest {
        model: request.model.clone(),
        messages: request.messages.iter().map(openai_message).collect(),
        tools: wire_tools(request, tools_supported),
        temperature,
        max_tokens,
    }
}

/// Project the wire `tools` array: empty unless the endpoint supports
/// function calling, with malformed definitions skipped.
///
/// When `tools_supported` is `false` the `tools` member degrades to absent
/// (graceful degrade), so an endpoint without function-calling support never
/// receives an incompatible payload. Tools with an empty name or a
/// non-object JSON Schema are skipped individually — the wire `parameters`
/// must be a JSON Schema object.
fn wire_tools(request: &AiRequest, tools_supported: bool) -> Vec<OpenAiWireTool> {
    if !tools_supported {
        return Vec::new();
    }
    request
        .tools
        .iter()
        .filter(|tool| !tool.name.is_empty() && tool.parameters.is_object())
        .map(|tool| OpenAiWireTool {
            r#type: "function",
            function: OpenAiWireFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        })
        .collect()
}

/// Project [`ModelConfig`] sampling overrides onto the wire fields.
///
/// Out-of-contract values degrade to `None` (the field is omitted) rather
/// than being sent: a non-finite or out-of-range temperature and a zero
/// `max_tokens` cap are never serialized.
fn sampling_params(config: Option<&ModelConfig>) -> (Option<f32>, Option<u32>) {
    let Some(config) = config else {
        return (None, None);
    };
    let temperature = config
        .temperature
        .filter(|value| value.is_finite() && (0.0..=2.0).contains(value));
    let max_tokens = config.max_tokens.filter(|value| *value > 0);
    (temperature, max_tokens)
}

/// Map one provider-independent message to an OpenAI chat message.
///
// Attachment payloads are rendered per the OpenAI contract: inline text file
// contents become part of the turn text; base64 images become `image_url`
// data-URI parts (FR-008).
fn openai_message(message: &AiMessage) -> OpenAiMessage {
    // Assistant agent turn with structured tool calls: content is `null` and
    // the calls ride the `tool_calls` array with verbatim argument strings.
    if message.role == AiRole::Assistant && !message.tool_calls.is_empty() {
        return OpenAiMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(
                message
                    .tool_calls
                    .iter()
                    .map(|call| OpenAiReqWireToolCall {
                        id: call.id.clone(),
                        r#type: "function",
                        function: OpenAiReqWireFunctionCall {
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
        };
    }
    // Tool result turn: role "tool" answering one call by id.
    if message.role == AiRole::Tool {
        let result = message.tool_result.as_ref().expect("tool result present");
        return OpenAiMessage {
            role: "tool".to_string(),
            content: Some(OpenAiContent::Text(result.content.clone())),
            tool_calls: None,
            tool_call_id: Some(result.call_id.clone()),
        };
    }
    let mut parts: Vec<OpenAiContentPart> = Vec::new();
    for attachment in &message.attachments {
        if let AiAttachmentPayload::Base64(data) = &attachment.payload {
            let mime = attachment
                .mime_type
                .as_deref()
                .unwrap_or("application/octet-stream");
            parts.push(OpenAiContentPart::ImageUrl {
                image_url: OpenAiImageUrl {
                    url: format!("data:{mime};base64,{data}"),
                },
            });
        }
    }
    // Plain string when there is nothing structural to send: the wire format
    // of attachment-free requests stays exactly as before.
    let content = if parts.is_empty() {
        OpenAiContent::Text(message.composed_content())
    } else {
        parts.insert(
            0,
            OpenAiContentPart::Text {
                text: message.composed_content(),
            },
        );
        OpenAiContent::Parts(parts)
    };
    OpenAiMessage {
        role: match message.role {
            AiRole::System => "system",
            AiRole::User => "user",
            AiRole::Assistant => "assistant",
            // Handled above; unreachable here but keeps the match exhaustive.
            AiRole::Tool => "tool",
        }
        .to_string(),
        content: Some(content),
        tool_calls: None,
        tool_call_id: None,
    }
}

/// A normalized OpenAI Chat Completions response.
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    model: String,
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiWireToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiWireToolCall {
    id: String,
    // Wire field consumed by serde, retained for OpenAI `type:"function"` compatibility.
    #[allow(dead_code)]
    r#type: String,
    function: OpenAiWireFunctionCall,
}

#[derive(Debug, Deserialize)]
struct OpenAiWireFunctionCall {
    name: String,
    arguments: String,
}
/// Normalize a successful OpenAI response into the provider-independent
/// [`AiResponse`], preserving the model that actually responded.
fn to_ai_response(response: ChatCompletionResponse) -> Result<AiResponse, OpenAiError> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(OpenAiError::UnexpectedResponse)?;
    let wire_tool_calls = choice.message.tool_calls.unwrap_or_default();
    let tool_calls: Vec<crate::application::execution::ToolCall> = wire_tool_calls
        .into_iter()
        .map(|call| crate::application::execution::ToolCall {
            id: call.id,
            name: call.function.name,
            arguments: call.function.arguments,
            // OpenAI responses carry no reasoning signature.
            thought_signature: None,
        })
        .collect();
    let content = if let Some(text) = choice.message.content {
        text
    } else if tool_calls.is_empty() {
        return Err(OpenAiError::UnexpectedResponse);
    } else {
        String::new()
    };
    let usage = response.usage.and_then(|u| {
        if u.prompt_tokens == 0 && u.completion_tokens == 0 {
            None
        } else {
            Some(crate::application::execution::TokenUsage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            })
        }
    });
    Ok(AiResponse {
        content,
        model: response.model,
        tool_calls,
        usage,
    })
}

/// Classify a non-success HTTP status into a secret-free failure category.
///
/// `retry_after_secs` is the merged provider `Retry-After` hint: the response
/// header first, then common JSON body shapes (see
/// [`super::transport::extract_retry_after`]), capped upstream. It is carried
/// only by the 429 [`OpenAiError::RateLimited`] category.
fn classify_status(status: u16, retry_after_secs: Option<u64>) -> OpenAiError {
    match status {
        400 | 404 => OpenAiError::InvalidRequest,
        401 | 403 => OpenAiError::Authentication,
        // 402 (OpenRouter insufficient credits/quota) is distinct from 401/403:
        // the credential is valid but the account cannot pay for this call.
        402 => OpenAiError::PaymentRequired,
        429 => OpenAiError::RateLimited { retry_after_secs },
        s if s >= 500 => OpenAiError::ProviderUnavailable,
        _ => OpenAiError::Provider,
    }
}

/// Classified OpenAI failure categories (secret-free).
///
/// These identify only the failure *category*; no credential, authorization
/// header, or request payload is ever stored. The provider-independent boundary
/// exposes only [`ExecutorError::Failure`] вЂ” this richer classification exists
/// so diagnostics can distinguish failure classes in the logs.
#[derive(Debug)]
enum OpenAiError {
    /// The OpenAI endpoint rejected the request as malformed (HTTP 400).
    InvalidRequest,
    /// The prompt exceeds the model context window: a client-error status
    /// whose body names the context window or token limit. Never retried;
    /// the agent loop treats it as a compaction trigger.
    ContextLengthExceeded,
    /// A network/transport failure (connection refused, DNS, timeout, ...).
    Network,
    /// The provider reported insufficient credits/quota (HTTP 402): the
    /// credential is valid but the account cannot pay for this call.
    PaymentRequired,
    /// The provider rate limited the request (HTTP 429), carrying the
    /// provider's `Retry-After` hint when it was a valid integer.
    RateLimited { retry_after_secs: Option<u64> },
    /// The provider is unavailable or overloaded (HTTP 5xx).
    ProviderUnavailable,
    /// The credential was rejected (HTTP 401).
    Authentication,
    /// A network failure or a provider-side error (HTTP 5xx, 429, ...).
    Provider,
    /// The response was not a recognizable chat completion (e.g. missing
    /// content or malformed JSON).
    UnexpectedResponse,
    /// The run was cancelled before the request completed: the in-flight
    /// attempt was aborted and no response was consumed.
    Cancelled,
}

impl std::fmt::Display for OpenAiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest => write!(f, "the OpenAI request was invalid (400)"),
            Self::ContextLengthExceeded => {
                write!(f, "the OpenAI prompt exceeds the model context window")
            }
            Self::Network => write!(f, "OpenAI network or transport failure"),
            Self::PaymentRequired => write!(
                f,
                "provider reported insufficient credits/quota (HTTP 402); \
                 top up or switch to a free-tier ID"
            ),
            Self::RateLimited { .. } => write!(f, "OpenAI rate limit (429)"),
            Self::ProviderUnavailable => write!(f, "OpenAI unavailable (5xx)"),
            Self::Authentication => write!(f, "OpenAI rejected the credential (401)"),
            Self::Provider => write!(f, "OpenAI provider or network failure"),
            Self::UnexpectedResponse => write!(f, "OpenAI returned an unexpected response"),
            Self::Cancelled => write!(f, "OpenAI request cancelled before completion"),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> AiRequest {
        AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![
                AiMessage {
                    role: AiRole::System,
                    content: "You are a helpful assistant.".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
                AiMessage {
                    role: AiRole::User,
                    content: "Hello".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
                AiMessage {
                    role: AiRole::Assistant,
                    content: "Hi there".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
            ],
            tools: Vec::new(),
            model_config: None,
            request_timeout: None,
        }
    }

    #[test]
    fn openrouter_models_match_smoke_gated_keep_list() {
        // 1.2.3 recovery: every ID below returned chat 200 on a live POST to
        // the OpenRouter `chat/completions` endpoint on 2026-09-13; the four
        // dropped IDs failed it (`minimax-m3:free`, `minimax-m2.7:free`,
        // `glm-5.2:free` chat 404, `ultra-550b-a55b:free` no HTTP response).
        assert_eq!(
            OPENROUTER_MODELS,
            &[
                "inclusionai/ling-3.0-flash-fin:free",
                "nvidia/nemotron-3.5-lightning:free",
                "nvidia/nemotron-3-super-120b-a12b:free",
                "cohere/north-mini-code:free",
                "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
                "inclusionai/ling-3.0-flash-sante:free",
                "inclusionai/ling-3.0-flash-vl:free",
                "liquid/lfm-2.5-2.6b:free",
            ]
        );
        assert_eq!(OPENROUTER_MODELS[0], "inclusionai/ling-3.0-flash-fin:free");
    }

    #[test]
    fn openrouter_tools_smoke_agent_usable_ids_are_supported() {
        // Every listed ID returned tools-leg 200 on the same live smoke run,
        // so the whole shortlist is agent-usable.
        for model in [
            "inclusionai/ling-3.0-flash-fin:free",
            "nvidia/nemotron-3.5-lightning:free",
            "nvidia/nemotron-3-super-120b-a12b:free",
            "cohere/north-mini-code:free",
            "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
            "inclusionai/ling-3.0-flash-sante:free",
            "inclusionai/ling-3.0-flash-vl:free",
            "liquid/lfm-2.5-2.6b:free",
        ] {
            assert!(
                OPENROUTER_MODELS.contains(&model),
                "tools 200 ID {model} must stay listed"
            );
        }
    }

    #[test]
    fn request_translates_roles_and_model() {
        let body = chat_completion_request(&sample_request(), true);
        // The selected model is passed through unchanged, never substituted.
        assert_eq!(body.model, "gpt-5.6-terra");
        let roles: Vec<&str> = body.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["system", "user", "assistant"]);
        assert_eq!(
            body.messages[0].content,
            Some(OpenAiContent::Text(
                "You are a helpful assistant.".to_string()
            ))
        );
        assert_eq!(
            body.messages[1].content,
            Some(OpenAiContent::Text("Hello".to_string()))
        );
        assert_eq!(
            body.messages[2].content,
            Some(OpenAiContent::Text("Hi there".to_string()))
        );
        // Messages remain in chronological order.
        assert_eq!(body.messages.len(), 3);
    }

    #[test]
    fn maps_user_and_assistant_roles_without_system() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-sol".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "ping".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }],
            tools: Vec::new(),
            model_config: None,
            request_timeout: None,
        };
        let body = chat_completion_request(&request, true);
        assert_eq!(body.messages[0].role, "user");
        assert_eq!(
            body.messages[0].content,
            Some(OpenAiContent::Text("ping".to_string()))
        );
    }

    #[test]
    fn response_maps_to_ai_response() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("Hello to you too.".to_string()),
                    tool_calls: None,
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("valid response maps");
        assert_eq!(ai.content, "Hello to you too.");
        assert_eq!(ai.model, "gpt-5.6-terra");
    }

    #[test]
    fn response_without_content_is_unexpected() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: None,
                    tool_calls: None,
                },
            }],
            usage: None,
        };
        assert!(matches!(
            to_ai_response(response),
            Err(OpenAiError::UnexpectedResponse)
        ));
    }

    #[test]
    fn response_without_choices_is_unexpected() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![],
            usage: None,
        };
        assert!(matches!(
            to_ai_response(response),
            Err(OpenAiError::UnexpectedResponse)
        ));
    }

    #[test]
    fn statuses_classify_without_secrets() {
        assert!(matches!(
            classify_status(400, None),
            OpenAiError::InvalidRequest
        ));
        assert!(matches!(
            classify_status(404, None),
            OpenAiError::InvalidRequest
        ));
        assert!(matches!(
            classify_status(401, None),
            OpenAiError::Authentication
        ));
        assert!(matches!(
            classify_status(403, None),
            OpenAiError::Authentication
        ));
        // 402 means the credential is valid but the account cannot pay for
        // the call (OpenRouter insufficient credits/quota).
        assert!(matches!(
            classify_status(402, None),
            OpenAiError::PaymentRequired
        ));
        // 429 carries the Retry-After hint (None when absent/not parseable).
        assert!(matches!(
            classify_status(429, None),
            OpenAiError::RateLimited {
                retry_after_secs: None
            }
        ));
        assert!(matches!(
            classify_status(500, None),
            OpenAiError::ProviderUnavailable
        ));
        assert!(matches!(
            classify_status(503, None),
            OpenAiError::ProviderUnavailable
        ));
        // Other 4xx remain the catch-all provider failure.
        assert!(matches!(classify_status(422, None), OpenAiError::Provider));
    }

    #[test]
    fn executor_maps_every_classified_failure_to_boundary_failure() {
        let executor = OpenAiExecutor {
            client: HttpClient::new(),
            endpoint: "http://127.0.0.1:1".to_string(), // unreachable -> network failure
            name: PROVIDER_NAME,
            extra_headers: Vec::new(),
            owned_headers: Vec::new(),
            organization: None,
            tools_supported: None,
        };
        // The boundary surfaces the classified category (here: network), never an
        // OpenAI-specific or secret-bearing type.
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(ExecutorError::Network)));
    }

    #[test]
    fn binary_attachments_serialize_as_openai_image_parts() {
        let mut request = sample_request();
        request.messages.push(AiMessage {
            role: AiRole::User,
            content: "What is in this image?".to_string(),
            attachments: vec![AiAttachment {
                file_name: "chart.png".to_string(),
                file_size_bytes: Some(4),
                mime_type: Some("image/png".to_string()),
                payload: AiAttachmentPayload::Base64("cG5nIQ==".to_string()),
            }],
            tool_calls: Vec::new(),
            tool_result: None,
        });
        let json =
            serde_json::to_string(&chat_completion_request(&request, true)).expect("serialize");

        // Text part plus an image_url part carrying a base64 data URI, per
        // the Chat Completions multimodal content contract.
        assert!(json.contains("\"type\":\"image_url\""));
        assert!(json.contains("data:image/png;base64,cG5nIQ=="));
        // No filesystem path can appear: the boundary never carries one.
        assert!(!json.contains("/tmp/"));
    }

    #[test]
    fn text_attachments_are_inlined_into_the_turn_text() {
        let mut request = sample_request();
        request.messages.push(AiMessage {
            role: AiRole::User,
            content: "Summarize".to_string(),
            attachments: vec![AiAttachment {
                file_name: "notes.txt".to_string(),
                file_size_bytes: Some(5),
                mime_type: Some("text/plain".to_string()),
                payload: AiAttachmentPayload::Text("revenue rose 12 percent".to_string()),
            }],
            tool_calls: Vec::new(),
            tool_result: None,
        });
        let body = chat_completion_request(&request, true);
        let user = body
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .expect("user message");
        let OpenAiContent::Text(text) = user.content.as_ref().expect("content present") else {
            panic!("text-only attachments keep the plain string wire shape");
        };
        assert!(text.contains("revenue rose 12 percent"));
    }

    #[test]
    fn executor_round_trips_through_local_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            // A minimal valid OpenAI-style success body.
            let body = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"pong"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");
        });

        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let ai = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect("round trip succeeds");
        server.join().expect("server thread joins");

        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "gpt-5.6-terra");
    }

    #[test]
    fn status_429_maps_to_rate_limited_with_retry_after() {
        // Case 1: 429 with a valid integer Retry-After header. The send is
        // retried up to the bounded attempt cap, so the scripted server must
        // answer all three attempts identically; the final error still carries
        // the header value. `Retry-After: 0` keeps the test fast (no backoff
        // sleep) while exercising the header-carry path.
        let (endpoint, _count, server) = spawn_sequence_server(vec![
            (429, String::new(), Some("0".to_string())),
            (429, String::new(), Some("0".to_string())),
            (429, String::new(), Some("0".to_string())),
        ]);
        let executor = OpenAiExecutor::with_endpoint(endpoint);
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(
            result,
            Err(ExecutorError::RateLimited {
                retry_after_secs: Some(0)
            })
        ));
        let _ = server.join();

        // Case 2: 429 without the header -> None (all three attempts).
        let (endpoint, _count, server) = spawn_sequence_server(vec![
            (429, String::new(), None),
            (429, String::new(), None),
            (429, String::new(), None),
        ]);
        let executor = OpenAiExecutor::with_endpoint(endpoint);
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(
            result,
            Err(ExecutorError::RateLimited {
                retry_after_secs: None
            })
        ));
        let _ = server.join();
    }

    #[test]
    fn status_5xx_maps_to_provider_unavailable() {
        for status in [500, 503] {
            // Bounded retry serves all three attempts with the same status.
            let (endpoint, _count, server) = spawn_sequence_server(vec![
                (status, String::new(), None),
                (status, String::new(), None),
                (status, String::new(), None),
            ]);
            let executor = OpenAiExecutor::with_endpoint(endpoint);
            let result = executor.execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            );
            assert!(
                matches!(result, Err(ExecutorError::ProviderUnavailable)),
                "status {status} should map to ProviderUnavailable, got {result:?}"
            );
            let _ = server.join();
        }
    }

    #[test]
    fn status_404_maps_to_invalid_request() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let response = "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(ExecutorError::InvalidRequest)));
        let _ = server.join();
    }

    #[test]
    fn status_402_maps_to_payment_required() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let response = "HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let err = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect_err("402 must fail");
        assert!(matches!(err, ExecutorError::PaymentRequired));
        // The boundary message names the remedy, never the body or credential.
        assert_eq!(
            err.to_string(),
            "provider reported insufficient credits/quota (HTTP 402); \
             top up or switch to a free-tier ID"
        );
        let _ = server.join();
    }

    #[test]
    fn other_client_errors_still_surface_as_failure() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let response = "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(ExecutorError::Failure)));
        let _ = server.join();
    }

    #[test]
    fn request_with_tools_serializes_with_function_type() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Use the tool".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }],
            tools: vec![crate::application::execution::ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get the weather for a location".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    },
                    "required": ["location"]
                }),
            }],
            model_config: None,
            request_timeout: None,
        };
        let json =
            serde_json::to_string(&chat_completion_request(&request, true)).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        // Tools key must be present with correct wire shape.
        let tools = value
            .get("tools")
            .expect("tools present")
            .as_array()
            .expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(
            tools[0]["function"]["description"],
            "Get the weather for a location"
        );
        assert!(tools[0]["function"]["parameters"]["properties"]["location"].is_object());
    }

    #[test]
    fn request_without_tools_omits_tools_key() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Hello".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }],
            tools: Vec::new(),
            model_config: None,
            request_timeout: None,
        };
        let json =
            serde_json::to_string(&chat_completion_request(&request, true)).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            value.get("tools").is_none(),
            "tools key must be absent when empty"
        );
        // Byte-for-byte backward compatible: no tools key at all.
        assert!(!json.contains("\"tools\""));
    }

    #[test]
    fn response_with_tool_calls_maps_to_ai_response() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: None,
                    tool_calls: Some(vec![OpenAiWireToolCall {
                        id: "call_123".to_string(),
                        r#type: "function".to_string(),
                        function: OpenAiWireFunctionCall {
                            name: "get_weather".to_string(),
                            arguments: "{\"location\":\"Paris\"}".to_string(),
                        },
                    }]),
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("valid tool call response maps");
        // Content defaults to empty string when only tool calls are present.
        assert_eq!(ai.content, "");
        assert_eq!(ai.model, "gpt-5.6-terra");
        assert_eq!(ai.tool_calls.len(), 1);
        assert_eq!(ai.tool_calls[0].id, "call_123");
        assert_eq!(ai.tool_calls[0].name, "get_weather");
        assert_eq!(ai.tool_calls[0].arguments, "{\"location\":\"Paris\"}");
    }

    #[test]
    fn response_with_content_and_tool_calls_maps_both() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("I will call the tool".to_string()),
                    tool_calls: Some(vec![OpenAiWireToolCall {
                        id: "call_456".to_string(),
                        r#type: "function".to_string(),
                        function: OpenAiWireFunctionCall {
                            name: "search".to_string(),
                            arguments: "{\"query\":\"test\"}".to_string(),
                        },
                    }]),
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("response with content and tool calls maps");
        assert_eq!(ai.content, "I will call the tool");
        assert_eq!(ai.tool_calls.len(), 1);
        assert_eq!(ai.tool_calls[0].name, "search");
    }

    #[test]
    fn plain_text_response_without_tools_still_maps_correctly() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("Hello to you too.".to_string()),
                    tool_calls: None,
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("plain text response maps");
        assert_eq!(ai.content, "Hello to you too.");
        assert_eq!(ai.model, "gpt-5.6-terra");
        assert!(
            ai.tool_calls.is_empty(),
            "plain text must have empty tool_calls"
        );
    }

    #[test]
    fn response_with_empty_tool_calls_and_content_maps_as_text() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("Just text".to_string()),
                    tool_calls: Some(vec![]),
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("empty tool_calls with content maps");
        assert_eq!(ai.content, "Just text");
        assert!(ai.tool_calls.is_empty());
    }

    /// The native tool round-trip serializes as the OpenAI shapes: an
    /// assistant turn with `"content": null` plus a `tool_calls` array
    /// carrying the verbatim argument string, and a tool-result turn with
    /// `role: "tool"`, `tool_call_id`, and the observation as text content.
    #[test]
    fn tool_round_trip_request_serializes_native_shapes() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![
                AiMessage {
                    role: AiRole::User,
                    content: "List files".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
                AiMessage {
                    role: AiRole::Assistant,
                    content: String::new(),
                    attachments: Vec::new(),
                    tool_calls: vec![crate::application::execution::ToolCall {
                        id: "call_7".to_string(),
                        name: "list_directory".to_string(),
                        arguments: r#"{"path":"."}"#.to_string(),
                        thought_signature: None,
                    }],
                    tool_result: None,
                },
                AiMessage {
                    role: AiRole::Tool,
                    content: String::new(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: Some(crate::application::execution::AiToolResult {
                        call_id: "call_7".to_string(),
                        name: "list_directory".to_string(),
                        content: "a.txt".to_string(),
                    }),
                },
            ],
            tools: Vec::new(),
            model_config: None,
            request_timeout: None,
        };
        let body = chat_completion_request(&request, true);
        let value: serde_json::Value =
            serde_json::to_value(&body).expect("request body serializes");

        assert_eq!(
            value["messages"][1],
            serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_7",
                        "type": "function",
                        "function": {
                            "name": "list_directory",
                            "arguments": "{\"path\":\".\"}"
                        }
                    }
                ]
            })
        );
        assert_eq!(
            value["messages"][2],
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call_7",
                "content": "a.txt"
            })
        );
    }

    /// Timeouts are transport failures and therefore retried per the bounded
    /// attempt cap (not fired once): three 200 ms attempts with the computed
    /// backoff between them, then a classified `Network` failure. The server
    /// accepts exactly three connections and answers none, so the hit count
    /// proves the bound and the elapsed time proves the backoff.
    #[test]
    fn request_timeout_is_threaded_through_send() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                seen.fetch_add(1, Ordering::SeqCst);
                let mut raw = Vec::new();
                let mut buf = [0u8; 1024];
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            raw.extend_from_slice(&buf[..n]);
                            if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                // Answer nothing: every attempt must hit the per-request
                // timeout instead of completing.
            }
        });
        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let mut request = sample_request();
        request.request_timeout = Some(Duration::from_millis(200));
        let start = std::time::Instant::now();
        let result = executor.execute(&request, "sk-secret-example", &CancellationToken::new());
        let elapsed = start.elapsed();
        server.join().expect("server thread joins");
        // Must be a classified boundary failure (timeout surfaces as Network).
        assert!(
            matches!(result, Err(ExecutorError::Network)),
            "expected timeout to surface as ExecutorError::Network, got {result:?}"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "a 200 ms timeout must be retried per the 3-attempt bound"
        );
        // Two computed backoffs separate the three attempts: [1.5, 2.5] s +
        // [3, 5] s on top of the 3 × 200 ms timeouts.
        assert!(
            elapsed >= Duration::from_millis(4_500),
            "retried timeouts must wait the computed backoff, elapsed={elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(30),
            "retried timeouts must stay bounded, elapsed={elapsed:?}"
        );
    }

    #[test]
    fn non_retryable_status_fires_once() {
        use std::sync::atomic::Ordering;
        // 400/401/403/404 each hit the local server exactly once: no retry,
        // no backoff sleep.
        for status in [400u16, 401, 403, 404] {
            let (endpoint, count, server) =
                spawn_sequence_server(vec![(status, String::new(), None)]);
            let executor = OpenAiExecutor::with_endpoint(endpoint);
            let result = executor.execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            );
            server.join().expect("server thread joins");
            match status {
                400 | 404 => assert!(
                    matches!(result, Err(ExecutorError::InvalidRequest)),
                    "status {status} must classify as InvalidRequest, got {result:?}"
                ),
                401 | 403 => assert!(
                    matches!(result, Err(ExecutorError::Authentication)),
                    "status {status} must classify as Authentication, got {result:?}"
                ),
                _ => unreachable!("scripted non-retryable status"),
            }
            assert_eq!(
                count.load(Ordering::SeqCst),
                1,
                "status {status} must fire exactly once"
            );
        }
    }

    /// Refused connections are retried per the bounded attempt cap, then
    /// surface as a classified `Network` failure. The endpoint is a port
    /// whose listener was just dropped, so every attempt refuses fast; the
    /// elapsed time (two computed backoffs) proves the retries happened.
    #[test]
    fn network_errors_are_retried_then_fail() {
        use std::net::TcpListener;
        use std::time::Duration;
        let closed = TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
        let addr = closed.local_addr().expect("local address");
        drop(closed);
        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let start = std::time::Instant::now();
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        let elapsed = start.elapsed();
        assert!(
            matches!(result, Err(ExecutorError::Network)),
            "exhausted refused connections must surface as Network, got {result:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(4_500),
            "refused connections must be retried with backoff, elapsed={elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(30),
            "exhausted retries must stay bounded, elapsed={elapsed:?}"
        );
    }

    /// Error bodies are read for classification hints, but nothing
    /// secret-bearing ever leaves the boundary: a 401 body carrying a live
    /// credential still classifies as `Authentication` with a category-only
    /// message, and a 429 body carrying both a secret and a `retry_after`
    /// field honors the hint (proving the body was read) without surfacing
    /// the secret.
    #[test]
    fn error_body_secret_never_leaves_boundary() {
        // Sentinel shapes that must never escape: live-key prefixes and the
        // exact secret values planted in the bodies below. ("credential" is
        // deliberately not among them: the fixed category text names the
        // credential-store concept — "rejected the stored credential" — while
        // the negative control here is the body-carried *values*.)
        const SECRET_SENTINELS: [&str; 5] = [
            "sk-",
            "secret",
            "api_key",
            "sk-live-sentinel-12345",
            "sk-live-sentinel-67890",
        ];
        let secret_body = "{\"error\":{\"message\":\"invalid api_key \
            sk-live-sentinel-12345, credential rejected\",\"type\":\"invalid_request_error\"}}"
            .to_string();

        // Case 1: 401 with a secret-carrying body.
        let (endpoint, _count, server) = spawn_sequence_server(vec![(401, secret_body, None)]);
        let executor = OpenAiExecutor::with_endpoint(endpoint);
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        server.join().expect("server thread joins");
        assert!(
            matches!(result, Err(ExecutorError::Authentication)),
            "secret-carrying 401 must classify as Authentication, got {result:?}"
        );
        let message = result.expect_err("401 must fail").to_string();
        for sentinel in SECRET_SENTINELS {
            assert!(
                !message.to_lowercase().contains(sentinel),
                "boundary message must not contain body-carried secret {sentinel:?}: {message:?}"
            );
        }

        // Case 2: 429 with a secret-carrying body that also carries a
        // `retry_after` field. The hint is honored (body was read: the final
        // error carries `Some(1)`), while the secret never surfaces.
        let sneaky = "{\"error\":{\"message\":\"rate limited, secret \
            sk-live-sentinel-67890\",\"retry_after\":1}}"
            .to_string();
        let (endpoint, _count, server) = spawn_sequence_server(vec![
            (429, sneaky.clone(), None),
            (429, sneaky.clone(), None),
            (429, sneaky, None),
        ]);
        let executor = OpenAiExecutor::with_endpoint(endpoint);
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        server.join().expect("server thread joins");
        assert!(
            matches!(
                result,
                Err(ExecutorError::RateLimited {
                    retry_after_secs: Some(1)
                })
            ),
            "body retry_after must be honored, got {result:?}"
        );
        let message = result.expect_err("429 must fail").to_string();
        for sentinel in SECRET_SENTINELS {
            assert!(
                !message.to_lowercase().contains(sentinel),
                "boundary message must not contain body-carried secret {sentinel:?}: {message:?}"
            );
        }
    }

    /// A cancelled run aborts an in-flight request promptly: the server holds
    /// the connection open for 30 s, the token fires at ~200 ms, and
    /// `execute` must report `Cancelled` in well under 5 s — never after the
    /// wall-clock timeout.
    #[test]
    fn cancel_aborts_in_flight_request_promptly() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let entered_clone = Arc::clone(&entered);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            entered_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            let _ = stream.set_read_timeout(Some(Duration::from_secs(35)));
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            // Hold the connection open far past any prompt-abort budget.
            std::thread::sleep(Duration::from_secs(30));
            let body =
                r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"too late"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });

        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let token = CancellationToken::new();
        let canceller = token.clone();
        let driver = std::thread::spawn(move || {
            while !entered.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(200));
            canceller.cancel();
        });
        let mut request = sample_request();
        request.request_timeout = Some(Duration::from_mins(2));
        let start = std::time::Instant::now();
        let result = executor.execute(&request, "sk-secret-example", &token);
        let elapsed = start.elapsed();
        driver.join().expect("canceller joins");
        // The server thread is still sleeping its 30 s; detach it rather than
        // joining so the test finishes promptly.
        std::mem::forget(server);
        assert!(
            matches!(result, Err(ExecutorError::Cancelled)),
            "cancelled in-flight request must report Cancelled, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "cancel must abort promptly, waited {elapsed:?}"
        );
    }
    #[test]
    fn usage_present_maps_to_token_usage() {
        let json = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).expect("parse");
        let usage = parsed.usage.clone().expect("usage present");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        let ai = to_ai_response(parsed).expect("to_ai");
        let u = ai.usage.expect("ai usage");
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 20);
    }

    #[test]
    fn usage_absent_maps_to_none() {
        let json = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"hi"}}]}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).expect("parse");
        assert!(parsed.usage.is_none());
        let ai = to_ai_response(parsed).expect("to_ai");
        assert!(ai.usage.is_none());
    }

    #[test]
    fn usage_zero_zero_maps_to_none() {
        let json = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":0,"completion_tokens":0}}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).expect("parse");
        let ai = to_ai_response(parsed).expect("to_ai");
        assert!(ai.usage.is_none(), "0,0 should map to None per spec");
    }

    #[test]
    fn usage_partial_zero_maps_to_some() {
        let json = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":0,"completion_tokens":5}}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).expect("parse");
        let ai = to_ai_response(parsed).expect("to_ai");
        let u = ai.usage.expect("some");
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 5);
    }

    #[test]
    fn compatible_executor_posts_to_given_endpoint() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            // A minimal valid OpenAI-style success body.
            let body = r#"{"model":"deepseek/deepseek-v4-flash","choices":[{"message":{"content":"pong"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");
        });

        let executor = OpenAiExecutor::compatible(XKIRO_NAME, format!("http://{addr}"));
        let ai = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect("round trip succeeds");
        server.join().expect("server thread joins");

        assert_eq!(ai.content, "pong");
    }

    #[test]
    fn compatible_openrouter_sends_referer_headers() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");

        let server = std::thread::spawn(move || -> Vec<u8> {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            // Read headers first, then the declared body so the credential
            // check below inspects the actual request payload.
            let mut header_end = None;
            let mut content_length = 0usize;
            while header_end.is_none() {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let head = String::from_utf8_lossy(&raw[..pos + 4]).to_lowercase();
                            for line in head.lines() {
                                if let Some(value) = line.strip_prefix("content-length:") {
                                    content_length = value.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                }
            }
            if let Some(end) = header_end {
                while raw.len() < end + content_length {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => raw.extend_from_slice(&buf[..n]),
                    }
                }
            }
            // A minimal valid OpenAI-style success body.
            let body =
                r#"{"model":"z-ai/glm-5.2:free","choices":[{"message":{"content":"pong"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");
            raw
        });

        let executor = OpenAiExecutor::compatible_with_headers(
            OPENROUTER_NAME,
            format!("http://{addr}"),
            &[
                ("HTTP-Referer", "https://github.com/sshdw/Nexora"),
                ("X-Title", "Nexora"),
            ],
        );
        let ai = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect("round trip succeeds");
        let raw = server.join().expect("server thread joins");

        assert_eq!(ai.content, "pong");
        // Header field names are case-insensitive on the wire (RFC 9110 §5.1)
        // and the HTTP stack normalizes them to lowercase; match the name
        // case-insensitively while requiring the exact header value bytes.
        let head_end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or(raw.len());
        let head = String::from_utf8_lossy(&raw[..head_end]);
        let has_header = |name: &str, value: &str| {
            head.lines().any(|line| {
                line.split_once(':')
                    .is_some_and(|(field_name, field_value)| {
                        field_name.eq_ignore_ascii_case(name) && field_value.trim() == value
                    })
            })
        };
        assert!(
            has_header("HTTP-Referer", "https://github.com/sshdw/Nexora"),
            "openrouter Referer header missing"
        );
        assert!(
            has_header("X-Title", "Nexora"),
            "openrouter Title header missing"
        );
        // The credential travels only in the Authorization header.
        let has_authorization = head.lines().any(|line| {
            line.split_once(':')
                .is_some_and(|(n, _)| n.eq_ignore_ascii_case("Authorization"))
        });
        assert!(has_authorization, "Authorization header missing");
        let body_part = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|pos| String::from_utf8_lossy(&raw[pos + 4..]).into_owned())
            .unwrap_or_default();
        assert!(
            !body_part.contains("sk-secret-example"),
            "request body must not include the credential"
        );
    }

    /// Spawn a local HTTP server that serves the scripted `responses` in order,
    /// one per accepted connection, and counts accepted connections.
    fn spawn_sequence_server(
        responses: Vec<(u16, String, Option<String>)>,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count_clone = std::sync::Arc::clone(&count);
        let server = std::thread::spawn(move || {
            for (status, body, retry_after) in responses {
                let (mut stream, _) = listener.accept().expect("accept connection");
                count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut raw = Vec::new();
                let mut buf = [0u8; 1024];
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            raw.extend_from_slice(&buf[..n]);
                            if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                let reason = match status {
                    400 => "Bad Request",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    503 => "Service Unavailable",
                    _ => "OK",
                };
                let retry_header = retry_after
                    .map(|value| format!("Retry-After: {value}\r\n"))
                    .unwrap_or_default();
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{retry_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), count, server)
    }

    #[test]
    fn retry_429_then_200_succeeds() {
        use std::sync::atomic::Ordering;
        let success =
            r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"pong"}}]}"#.to_string();
        let (endpoint, count, server) = spawn_sequence_server(vec![
            (429, String::new(), Some("0".to_string())),
            (200, success, None),
        ]);
        let executor = OpenAiExecutor::with_endpoint(endpoint);
        let ai = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect("429-then-200 must succeed");
        server.join().expect("server thread joins");
        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "gpt-5.6-terra");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retry_503_then_200_succeeds() {
        use std::sync::atomic::Ordering;
        let success =
            r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"pong"}}]}"#.to_string();
        let (endpoint, count, server) = spawn_sequence_server(vec![
            (503, String::new(), Some("0".to_string())),
            (200, success, None),
        ]);
        let executor = OpenAiExecutor::with_endpoint(endpoint);
        let ai = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect("503-then-200 must succeed");
        server.join().expect("server thread joins");
        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "gpt-5.6-terra");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retry_400_is_never_retried() {
        use std::sync::atomic::Ordering;
        let (endpoint, count, server) = spawn_sequence_server(vec![(400, String::new(), None)]);
        let executor = OpenAiExecutor::with_endpoint(endpoint);
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        server.join().expect("server thread joins");
        assert!(matches!(result, Err(ExecutorError::InvalidRequest)));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn status_400_with_context_body_maps_to_context_length_exceeded() {
        use std::sync::atomic::Ordering;
        // A 400 naming the context window is a compaction trigger, not a
        // malformed request — and, like every 400, it fires exactly once.
        let body = r#"{"error":{"message":"This model's maximum context length is 128000 tokens","type":"invalid_request_error","code":"context_length_exceeded"}}"#.to_string();
        let (endpoint, count, server) = spawn_sequence_server(vec![(400, body, None)]);
        let executor = OpenAiExecutor::with_endpoint(endpoint);
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        server.join().expect("server thread joins");
        assert!(
            matches!(result, Err(ExecutorError::ContextLengthExceeded)),
            "context-length 400 must classify as ContextLengthExceeded, got {result:?}"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retry_attempts_are_capped_at_three() {
        use std::sync::atomic::Ordering;
        // `Retry-After: 0` keeps this a cap proof, not a timing proof: the
        // attempt bound and classification are what matter here, and a zero
        // hint retries immediately. Real-backoff timing is proven once in the
        // transport `retry_after_*` test instead of slept out per provider.
        let (endpoint, count, server) = spawn_sequence_server(vec![
            (503, String::new(), Some("0".to_string())),
            (503, String::new(), Some("0".to_string())),
            (503, String::new(), Some("0".to_string())),
        ]);
        let executor = OpenAiExecutor::with_endpoint(endpoint);
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        server.join().expect("server thread joins");
        assert!(matches!(result, Err(ExecutorError::ProviderUnavailable)));
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    fn valid_compat_config() -> CompatConfig {
        CompatConfig {
            base_url: "https://proxy.example.com/v1/chat/completions".to_string(),
            model: "custom-model".to_string(),
            organization: None,
            headers: Vec::new(),
            supports_tools: true,
        }
    }

    #[test]
    fn compat_config_valid_accepts_http_and_https() {
        for url in [
            "https://proxy.example.com/v1/chat/completions",
            "http://127.0.0.1:8080/v1/chat/completions",
            "http://localhost:11434/v1/chat/completions",
            "https://proxy.example.com:443/chat/completions?key=q",
            "http://[::1]:8080/v1/chat/completions",
        ] {
            let mut config = valid_compat_config();
            config.base_url = url.to_string();
            assert!(config.validate().is_ok(), "base URL {url:?} must validate");
        }
    }

    #[test]
    fn compat_config_rejects_bad_urls_without_echoing_them() {
        // (candidate URL, expected category). The URL itself — which may
        // embed credentials — is never echoed in the error text.
        for (url, expected) in [
            ("", CompatConfigError::MissingBaseUrl),
            ("   ", CompatConfigError::MissingBaseUrl),
            (
                "ftp://proxy.example.com/v1",
                CompatConfigError::UnsupportedScheme,
            ),
            ("proxy.example.com/v1", CompatConfigError::UnsupportedScheme),
            ("https://", CompatConfigError::MissingHost),
            ("https:///v1/chat", CompatConfigError::MissingHost),
            (
                "https://user:pass@proxy.example.com/v1",
                CompatConfigError::CredentialsInUrl,
            ),
            (
                "https://user@proxy.example.com/v1",
                CompatConfigError::CredentialsInUrl,
            ),
            (
                "https://proxy.example.com:0/v1",
                CompatConfigError::InvalidPort,
            ),
            (
                "https://proxy.example.com:99999/v1",
                CompatConfigError::InvalidPort,
            ),
            (
                "https://proxy.example.com:abc/v1",
                CompatConfigError::MissingHost,
            ),
            (
                "https://proxy.example .com/v1",
                CompatConfigError::InvalidCharacters,
            ),
            (
                "https://proxy.example.com/v1\tchat",
                CompatConfigError::InvalidCharacters,
            ),
        ] {
            let mut config = valid_compat_config();
            config.base_url = url.to_string();
            let err = config.validate().expect_err("bad URL must fail");
            assert_eq!(err, expected, "URL {url:?} must classify as {expected:?}");
            let message = err.to_string();
            assert!(
                !message.contains("pass") && !message.contains("user"),
                "error text must not echo the URL: {message:?}"
            );
        }
        // An overlong URL fails by category, not by content.
        let mut config = valid_compat_config();
        config.base_url = format!("https://example.com/{}", "a".repeat(2_048));
        assert_eq!(config.validate(), Err(CompatConfigError::BaseUrlTooLong));
    }

    #[test]
    fn compat_config_rejects_bad_models_organizations_and_headers() {
        let mut config = valid_compat_config();
        config.model = String::new();
        assert_eq!(config.validate(), Err(CompatConfigError::MissingModel));
        config.model = "a".repeat(257);
        assert_eq!(config.validate(), Err(CompatConfigError::ModelTooLong));

        let mut config = valid_compat_config();
        config.organization = Some("org with spaces".to_string());
        assert_eq!(
            config.validate(),
            Err(CompatConfigError::InvalidOrganization)
        );
        config.organization = Some("org-valid_123".to_string());
        assert!(config.validate().is_ok());

        // Reserved transport headers cannot be shadowed.
        for name in ["Authorization", "content-type", "OpenAI-Organization"] {
            let mut config = valid_compat_config();
            config.headers = vec![(name.to_string(), "value".to_string())];
            assert_eq!(
                config.validate(),
                Err(CompatConfigError::InvalidHeaderName),
                "reserved header {name:?} must be rejected"
            );
        }
        // CR/LF injection in a value is rejected by category only.
        let mut config = valid_compat_config();
        config.headers = vec![("X-Custom".to_string(), "a\r\nb".to_string())];
        assert_eq!(
            config.validate(),
            Err(CompatConfigError::InvalidHeaderValue)
        );
        // A custom header value that looks like a secret still validates —
        // and no error path ever echoes it (there is no error here at all).
        let mut config = valid_compat_config();
        config.headers = vec![("X-Custom".to_string(), "sk-live-sentinel-999".to_string())];
        assert!(config.validate().is_ok());

        let mut config = valid_compat_config();
        config.headers = (0..17)
            .map(|index| (format!("X-H{index}"), "v".to_string()))
            .collect();
        assert_eq!(config.validate(), Err(CompatConfigError::TooManyHeaders));
    }

    #[test]
    fn compat_field_presence_reports_booleans_only() {
        let empty = CompatConfig::default();
        let presence = empty.field_presence();
        assert!(!presence.has_base_url);
        assert!(!presence.base_url_valid);
        assert!(!presence.has_model);
        assert!(!presence.has_organization);
        assert_eq!(presence.header_count, 0);

        let full = CompatConfig {
            base_url: "https://proxy.example.com/v1/chat/completions".to_string(),
            model: "m".to_string(),
            organization: Some("org-1".to_string()),
            headers: vec![("X-A".to_string(), "b".to_string())],
            supports_tools: true,
        };
        let presence = full.field_presence();
        assert!(presence.has_base_url);
        assert!(presence.base_url_valid);
        assert!(presence.has_model);
        assert!(presence.has_organization);
        assert_eq!(presence.header_count, 1);
    }

    #[test]
    fn tool_calling_degrades_when_endpoint_lacks_support() {
        let request = AiRequest {
            provider: COMPAT_NAME.to_string(),
            model: "custom-model".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Use the tool".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }],
            tools: vec![crate::application::execution::ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get the weather".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            model_config: None,
            request_timeout: None,
        };
        // Supported: tools ride the wire.
        let json =
            serde_json::to_string(&chat_completion_request(&request, true)).expect("serialize");
        assert!(json.contains("\"tools\""));
        // Unsupported: the member degrades to absent — byte-compatible with
        // a text-only request.
        let json =
            serde_json::to_string(&chat_completion_request(&request, false)).expect("serialize");
        assert!(
            !json.contains("\"tools\""),
            "unsupported endpoint must not receive tools: {json}"
        );
    }

    #[test]
    fn malformed_tool_definitions_are_skipped_individually() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "hi".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }],
            tools: vec![
                crate::application::execution::ToolDefinition {
                    name: "good".to_string(),
                    description: "fine".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                },
                crate::application::execution::ToolDefinition {
                    name: String::new(),
                    description: "nameless".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                },
                crate::application::execution::ToolDefinition {
                    name: "bad-schema".to_string(),
                    description: "array schema".to_string(),
                    parameters: serde_json::json!([{"type": "string"}]),
                },
            ],
            model_config: None,
            request_timeout: None,
        };
        let value: serde_json::Value =
            serde_json::to_value(chat_completion_request(&request, true)).expect("serialize");
        let tools = value["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "good");
    }

    #[test]
    fn sampling_params_serialize_only_when_in_contract() {
        let mut request = sample_request();
        // Absent config: both members omitted (historical wire shape).
        let json =
            serde_json::to_string(&chat_completion_request(&request, true)).expect("serialize");
        assert!(!json.contains("temperature"));
        assert!(!json.contains("max_tokens"));

        // In-contract values ride the wire.
        request.model_config = Some(
            ModelConfig::new("gpt-5.6-terra")
                .with_temperature(Some(0.7))
                .with_max_tokens(Some(512)),
        );
        let value: serde_json::Value =
            serde_json::to_value(chat_completion_request(&request, true)).expect("serialize");
        // `temperature` is an `f32` on the wire: compare within one unit in
        // the last place rather than against the `f64` literal.
        let temperature = value["temperature"].as_f64().expect("temperature number");
        assert!(
            (temperature - 0.7).abs() < 1e-6,
            "temperature must round-trip, got {temperature}"
        );
        assert_eq!(value["max_tokens"], 512);

        // Out-of-contract values degrade to absent, never sent.
        for (temperature, max_tokens) in [
            (Some(f32::NAN), Some(1)),
            (Some(-0.5), Some(1)),
            (Some(2.5), Some(1)),
            (Some(f32::INFINITY), Some(1)),
            (Some(0.7), Some(0)),
        ] {
            request.model_config = Some(
                ModelConfig::new("gpt-5.6-terra")
                    .with_temperature(temperature)
                    .with_max_tokens(max_tokens),
            );
            let json =
                serde_json::to_string(&chat_completion_request(&request, true)).expect("serialize");
            assert!(
                !json.contains("temperature")
                    || temperature.is_some_and(|t| (0.0..=2.0).contains(&t)),
                "out-of-range temperature must be omitted: {json}"
            );
            if max_tokens == Some(0) {
                assert!(
                    !json.contains("max_tokens"),
                    "zero max_tokens must be omitted: {json}"
                );
            }
        }
    }

    #[test]
    fn compat_executor_sends_organization_and_custom_headers() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let server = std::thread::spawn(move || -> Vec<u8> {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            let mut header_end = None;
            let mut content_length = 0usize;
            while header_end.is_none() {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let head = String::from_utf8_lossy(&raw[..pos + 4]).to_lowercase();
                            for line in head.lines() {
                                if let Some(value) = line.strip_prefix("content-length:") {
                                    content_length = value.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                }
            }
            if let Some(end) = header_end {
                while raw.len() < end + content_length {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => raw.extend_from_slice(&buf[..n]),
                    }
                }
            }
            let body = r#"{"model":"custom-model","choices":[{"message":{"content":"pong"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");
            raw
        });

        let config = CompatConfig {
            base_url: format!("http://{addr}"),
            model: "custom-model".to_string(),
            organization: Some("org-test".to_string()),
            headers: vec![("X-Custom".to_string(), "custom-value".to_string())],
            supports_tools: true,
        };
        assert!(config.validate().is_ok());
        let executor = OpenAiExecutor::compatible_with_config(COMPAT_NAME, &config);
        let ai = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect("round trip succeeds");
        let raw = server.join().expect("server thread joins");
        assert_eq!(ai.content, "pong");

        let head_end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or(raw.len());
        let head = String::from_utf8_lossy(&raw[..head_end]);
        let has_header = |name: &str, value: &str| {
            head.lines().any(|line| {
                line.split_once(':')
                    .is_some_and(|(field_name, field_value)| {
                        field_name.eq_ignore_ascii_case(name) && field_value.trim() == value
                    })
            })
        };
        assert!(
            has_header("OpenAI-Organization", "org-test"),
            "organization header missing"
        );
        assert!(
            has_header("X-Custom", "custom-value"),
            "custom header missing"
        );
        assert!(
            has_header("Authorization", "Bearer sk-secret-example"),
            "bearer credential must ride Authorization"
        );
    }

    #[test]
    fn compat_executor_with_invalid_endpoint_fails_closed_without_network() {
        // An unvalidated config can never send: the run fails closed as
        // `InvalidRequest` before any network activity.
        let config = CompatConfig {
            base_url: "ftp://proxy.example.com/v1".to_string(),
            ..valid_compat_config()
        };
        let executor = OpenAiExecutor::compatible_with_config(COMPAT_NAME, &config);
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(ExecutorError::InvalidRequest)));
    }

    #[test]
    fn compat_validation_errors_carry_no_secrets() {
        // Sentinel secret material that must never appear in any message,
        // mirroring the boundary secret-hygiene proof above.
        const SECRET_SENTINELS: [&str; 4] = [
            "sk-live-sentinel-compat",
            "s3cr3t-value",
            "passw0rd",
            "api_key",
        ];
        let config = CompatConfig {
            base_url: "https://user:sk-live-sentinel-compat@proxy.example.com/v1".to_string(),
            model: "m".to_string(),
            organization: None,
            headers: vec![(
                "X-K".to_string(),
                "s3cr3t-value passw0rd api_key".to_string(),
            )],
            supports_tools: true,
        };
        let err = config.validate().expect_err("credentialed URL must fail");
        assert_eq!(err, CompatConfigError::CredentialsInUrl);
        let message = err.to_string();
        for sentinel in SECRET_SENTINELS {
            assert!(
                !message.to_lowercase().contains(sentinel),
                "validation message must not echo secrets: {message:?}"
            );
        }
    }
}
