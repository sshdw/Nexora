//! The provider catalog: hardcoded provider identities, supported model
//! lists, and rate tables (AC-302).
//!
//! Pure data, extracted verbatim from the protocol executor modules so the
//! catalog has one home (AGENTS.md names the `SUPPORTED_MODELS` consts as the
//! sync point with the model-catalog doc). Each provider slice is re-exported
//! by its executor module (`gemini.rs`, `openai.rs`), so every existing
//! consumer path - the `supported_providers` aggregation in `mod.rs`,
//! `registry`, `context_stats`, `privacy`, and the commands - compiles
//! unchanged. Anthropic keeps its consts inline in `anthropic.rs` (AC-302
//! names only the OpenAI and Gemini executors).

// The module docs deliberately reference product/brand names (OpenAI, Gemini,
// xKiro, OpenRouter, NVIDIA, OpenCode Zen), which the `doc_markdown` pedantic
// lint flags as needing backticks. Allow it locally, matching the executors.
#![allow(clippy::doc_markdown)]

/// Gemini catalog slice: provider identity, supported models, the
/// live-verified lite-window subset, and the rate table. Re-exported by
/// the `gemini` executor module.
pub(crate) mod gemini {
    /// Internal provider name (DATABASE.md В§7.5); the keyring namespace key.
    pub(crate) const PROVIDER_NAME: &str = "gemini";

    /// User-facing provider label.
    pub(crate) const PROVIDER_DISPLAY_NAME: &str = "Gemini";

    /// Gemini models currently supported by the provider (DATABASE.md В§7.5:
    /// model lists are hardcoded in the MVP and managed by the application layer).
    ///
    /// The selected model is passed through unchanged and is never validated
    /// against this list at runtime (never silently substituted, never rejected):
    /// this set documents the currently supported models and anchors the
    /// model-selection tests.
    ///
    /// Live-smoke-gated 1.2.2 set (rotated key, 2026-09-13; D8 rule: chat 200
    /// keep, tools 200 agent-usable, 429 stays, 404/503 dropped):
    /// `gemini-3.6-flash` (default), `gemini-3.1-flash-lite`,
    /// `gemini-3.1-pro-preview` (429), `gemini-flash-lite-latest`,
    /// `gemini-pro-latest` (429), `gemini-3.5-flash`, `gemini-3.5-flash-lite`.
    /// Dropped: `gemini-2.5-flash`, `gemini-2.5-flash-lite`, `gemini-2.5-pro`
    /// (404), `gemini-flash-latest` (503).
    ///
    /// Pricing is governed by the application-layer pricing policy table
    /// (DATABASE.md В§7.8); the first entry
    /// is the provider default consumed as `models[0]` by the selection surface.
    /// The retired IDs this list replaces: `gemini-1.5-pro`, `gemini-1.5-flash`
    /// (shut down), and `gemini-2.0-flash` (retired June 1, 2026).
    pub(crate) const SUPPORTED_MODELS: &[&str] = &[
        // Default: chat 200 + tools 200.
        "gemini-3.6-flash",
        // Chat 200 + tools 200.
        "gemini-3.1-flash-lite",
        // Chat 429 stays per D8.
        "gemini-3.1-pro-preview",
        // Chat 200 + tools 200.
        "gemini-flash-lite-latest",
        // Chat 429 stays per D8.
        "gemini-pro-latest",
        // Chat 200 + tools 200.
        "gemini-3.5-flash",
        // Chat 200 + tools 200.
        "gemini-3.5-flash-lite",
    ];

    /// The [`SUPPORTED_MODELS`] subset on the live-verified lite window.
    ///
    /// Live-verified 2026-10-03 via the zero-token `GET
    /// /v1beta/models/{modelId}` metadata endpoint: every ID here reports
    /// `inputTokenLimit = 1048576`. Kept as an explicit subset (rather than
    /// re-derived by callers) so the application context-window table resolves
    /// the verified clamp without
    /// restating IDs — a strict subset of [`SUPPORTED_MODELS`] by construction
    /// (see the `gemini_lite_subset_of_supported` test).
    pub(crate) const GEMINI_LITE_MODELS: &[&str] = &[
        "gemini-3.1-flash-lite",
        "gemini-flash-lite-latest",
        "gemini-3.5-flash",
        "gemini-3.5-flash-lite",
    ];

    /// Estimated cost rates per 1M tokens, micro-USD (`(model, input, output)`).
    ///
    /// Hardcoded estimates in abstract micro-USD units (1 USD = `1_000_000`
    /// micro-USD), matching `RunBudget`.
    /// These are planning estimates — not live provider pricing, never fetched
    /// over the network — and only cover the native [`SUPPORTED_MODELS`] IDs.
    /// Unknown IDs yield `None` from [`crate::infrastructure::providers::gemini::rate_for_model`]: callers fall back to
    /// the policy default, never a guessed rate.
    pub(crate) const MODEL_RATES: &[(&str, u64, u64)] = &[
        ("gemini-3.6-flash", 1_250_000, 5_000_000),
        ("gemini-3.1-flash-lite", 300_000, 1_200_000),
        ("gemini-3.1-pro-preview", 2_500_000, 10_000_000),
        ("gemini-flash-lite-latest", 300_000, 1_200_000),
        ("gemini-pro-latest", 2_500_000, 10_000_000),
        ("gemini-3.5-flash", 1_250_000, 5_000_000),
        ("gemini-3.5-flash-lite", 300_000, 1_200_000),
    ];
}

/// OpenAI-family catalog slice: provider identity, supported models, the
/// rate table, and the four OpenAI-compatible endpoint identities (xKiro,
/// OpenRouter, NVIDIA NIM, OpenCode Zen) plus the user-configured endpoint
/// identity. Re-exported by the `openai` executor module.
pub(crate) mod openai {
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
    /// Pricing is governed by the application-layer pricing policy table
    /// (DATABASE.md В§7.8); the first entry
    /// is the provider default consumed as `models[0]` by the selection surface.
    pub(crate) const SUPPORTED_MODELS: &[&str] = &[
        // Default: best balance of cost and capability.
        "gpt-5.6-terra",
        // Fast/cheap tier.
        "gpt-5.6-luna",
        // Best-quality flagship tier.
        "gpt-5.6-sol",
    ];

    /// Estimated cost rates per 1M tokens, micro-USD (`(model, input, output)`).
    ///
    /// Hardcoded estimates in abstract micro-USD units (1 USD = `1_000_000`
    /// micro-USD), matching `RunBudget`.
    /// These are planning estimates — not live provider pricing, never fetched
    /// over the network — and only cover the native [`SUPPORTED_MODELS`] IDs.
    /// Unknown IDs (including every OpenAI-compatible shortlist below) yield
    /// `None` from [`crate::infrastructure::providers::openai::rate_for_model`]: callers fall back to the policy default,
    /// never a guessed rate.
    ///
    /// The default tier (`gpt-5.6-terra`) is rated exactly at the conservative
    /// policy default, so it bills identically rated or unrated.
    pub(crate) const MODEL_RATES: &[(&str, u64, u64)] = &[
        ("gpt-5.6-terra", 5_000_000, 25_000_000),
        ("gpt-5.6-luna", 500_000, 2_000_000),
        ("gpt-5.6-sol", 5_000_000, 20_000_000),
    ];
    /// OpenAI-compatible provider identities sharing [`crate::infrastructure::providers::openai::OpenAiExecutor`].
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
    /// shared [`crate::infrastructure::providers::openai::OpenAiExecutor`].
    pub(crate) const COMPAT_NAME: &str = "openai_compat";
    /// User-facing label for the configurable endpoint.
    pub(crate) const COMPAT_DISPLAY_NAME: &str = "OpenAI Compatible";
}
