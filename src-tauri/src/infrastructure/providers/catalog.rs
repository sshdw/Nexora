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
    /// Per-model baseline capability specification for curated presets.
    ///
    /// Anchors context windows, reasoning efforts, thinking, attachment, and
    /// tool support for flagship models until overridden by live `/models`
    /// metadata or an explicit user override (`ManualOverride > ApiMetadata >
    /// BuiltInCatalog > Unknown`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct BaselineModelSpec {
        pub model: &'static str,
        pub context_window: u32,
        pub supported_efforts: &'static [&'static str],
        pub supports_thinking: bool,
        pub supports_attachments: bool,
        pub supports_tools: bool,
    }

    /// Data-driven descriptor for an OpenAI-compatible provider preset.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct OpenAiPresetDescriptor {
        pub name: &'static str,
        pub display_name: &'static str,
        pub endpoint: &'static str,
        pub host: &'static str,
        pub extra_headers: &'static [(&'static str, &'static str)],
        pub models: &'static [&'static str],
        pub baseline_specs: &'static [BaselineModelSpec],
    }

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
    pub(crate) const OPENROUTER_HEADERS: &[(&str, &str)] = &[
        ("HTTP-Referer", "https://github.com/sshdw/Nexora"),
        ("X-Title", "Nexora"),
    ];
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

    pub(crate) const DEEPSEEK_NAME: &str = "deepseek";
    pub(crate) const DEEPSEEK_DISPLAY_NAME: &str = "DeepSeek";
    pub(crate) const DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com/v1/chat/completions";
    pub(crate) const DEEPSEEK_MODELS: &[&str] = &["deepseek-chat", "deepseek-reasoner"];
    const DEEPSEEK_SPECS: &[BaselineModelSpec] = &[
        BaselineModelSpec {
            model: "deepseek-chat",
            context_window: 128_000,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "deepseek-reasoner",
            context_window: 128_000,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
    ];

    pub(crate) const QWEN_NAME: &str = "qwen";
    pub(crate) const QWEN_DISPLAY_NAME: &str = "Alibaba / Qwen";
    pub(crate) const QWEN_ENDPOINT: &str =
        "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions";
    pub(crate) const QWEN_MODELS: &[&str] = &["qwen-max", "qwen-plus", "qwq-plus", "qwen-vl-max"];
    const QWEN_SPECS: &[BaselineModelSpec] = &[
        BaselineModelSpec {
            model: "qwen-max",
            context_window: 128_000,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "qwen-plus",
            context_window: 131_072,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "qwq-plus",
            context_window: 131_072,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "qwen-vl-max",
            context_window: 128_000,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: true,
            supports_tools: true,
        },
    ];

    pub(crate) const ZAI_NAME: &str = "zai";
    pub(crate) const ZAI_DISPLAY_NAME: &str = "ZAI";
    pub(crate) const ZAI_ENDPOINT: &str = "https://api.z.ai/api/paas/v4/chat/completions";
    pub(crate) const ZAI_MODELS: &[&str] = &["glm-4.5", "glm-4.5-air", "glm-4-plus"];
    const ZAI_SPECS: &[BaselineModelSpec] = &[
        BaselineModelSpec {
            model: "glm-4.5",
            context_window: 128_000,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "glm-4.5-air",
            context_window: 128_000,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "glm-4-plus",
            context_window: 128_000,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
    ];

    pub(crate) const MINIMAX_NAME: &str = "minimax";
    pub(crate) const MINIMAX_DISPLAY_NAME: &str = "MiniMax";
    pub(crate) const MINIMAX_ENDPOINT: &str = "https://api.minimax.io/v1/chat/completions";
    pub(crate) const MINIMAX_MODELS: &[&str] = &["MiniMax-M1", "MiniMax-Text-01"];
    const MINIMAX_SPECS: &[BaselineModelSpec] = &[
        BaselineModelSpec {
            model: "MiniMax-M1",
            context_window: 1_000_000,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "MiniMax-Text-01",
            context_window: 1_000_000,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
    ];

    pub(crate) const MOONSHOT_NAME: &str = "moonshot";
    pub(crate) const MOONSHOT_DISPLAY_NAME: &str = "Moonshot / Kimi";
    pub(crate) const MOONSHOT_ENDPOINT: &str = "https://api.moonshot.ai/v1/chat/completions";
    pub(crate) const MOONSHOT_MODELS: &[&str] = &[
        "kimi-k2-0711-preview",
        "kimi-thinking-preview",
        "moonshot-v1-128k",
        "moonshot-v1-128k-vision-preview",
    ];
    const MOONSHOT_SPECS: &[BaselineModelSpec] = &[
        BaselineModelSpec {
            model: "kimi-k2-0711-preview",
            context_window: 128_000,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "kimi-thinking-preview",
            context_window: 128_000,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "moonshot-v1-128k",
            context_window: 128_000,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "moonshot-v1-128k-vision-preview",
            context_window: 128_000,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: true,
            supports_tools: true,
        },
    ];

    pub(crate) const GROQ_NAME: &str = "groq";
    pub(crate) const GROQ_DISPLAY_NAME: &str = "Groq";
    pub(crate) const GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
    pub(crate) const GROQ_MODELS: &[&str] = &[
        "llama-3.3-70b-versatile",
        "deepseek-r1-distill-llama-70b",
        "meta-llama/llama-4-scout-17b-16e-instruct",
        "qwen-qwq-32b",
    ];
    const GROQ_SPECS: &[BaselineModelSpec] = &[
        BaselineModelSpec {
            model: "llama-3.3-70b-versatile",
            context_window: 131_072,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "deepseek-r1-distill-llama-70b",
            context_window: 131_072,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "meta-llama/llama-4-scout-17b-16e-instruct",
            context_window: 131_072,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: true,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "qwen-qwq-32b",
            context_window: 131_072,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
    ];

    pub(crate) const XAI_NAME: &str = "xai";
    pub(crate) const XAI_DISPLAY_NAME: &str = "xAI";
    pub(crate) const XAI_ENDPOINT: &str = "https://api.x.ai/v1/chat/completions";
    pub(crate) const XAI_MODELS: &[&str] = &["grok-3", "grok-3-mini", "grok-2-vision-1212"];
    const XAI_SPECS: &[BaselineModelSpec] = &[
        BaselineModelSpec {
            model: "grok-3",
            context_window: 131_072,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "grok-3-mini",
            context_window: 131_072,
            supported_efforts: &["low", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "grok-2-vision-1212",
            context_window: 32_768,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: true,
            supports_tools: true,
        },
    ];

    pub(crate) const MISTRAL_NAME: &str = "mistral";
    pub(crate) const MISTRAL_DISPLAY_NAME: &str = "Mistral";
    pub(crate) const MISTRAL_ENDPOINT: &str = "https://api.mistral.ai/v1/chat/completions";
    pub(crate) const MISTRAL_MODELS: &[&str] = &[
        "mistral-large-latest",
        "magistral-medium-latest",
        "codestral-latest",
        "pixtral-large-latest",
    ];
    const MISTRAL_SPECS: &[BaselineModelSpec] = &[
        BaselineModelSpec {
            model: "mistral-large-latest",
            context_window: 131_072,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "magistral-medium-latest",
            context_window: 128_000,
            supported_efforts: &["low", "medium", "high"],
            supports_thinking: true,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "codestral-latest",
            context_window: 256_000,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: false,
            supports_tools: true,
        },
        BaselineModelSpec {
            model: "pixtral-large-latest",
            context_window: 131_072,
            supported_efforts: &[],
            supports_thinking: false,
            supports_attachments: true,
            supports_tools: true,
        },
    ];

    /// Single data-driven descriptor table for all curated OpenAI-compatible presets.
    pub(crate) const OPENAI_COMPAT_PRESETS: &[OpenAiPresetDescriptor] = &[
        OpenAiPresetDescriptor {
            name: XKIRO_NAME,
            display_name: XKIRO_DISPLAY_NAME,
            endpoint: XKIRO_ENDPOINT,
            host: "api.xkiro.com",
            extra_headers: &[],
            models: XKIRO_MODELS,
            baseline_specs: &[],
        },
        OpenAiPresetDescriptor {
            name: OPENROUTER_NAME,
            display_name: OPENROUTER_DISPLAY_NAME,
            endpoint: OPENROUTER_ENDPOINT,
            host: "openrouter.ai",
            extra_headers: OPENROUTER_HEADERS,
            models: OPENROUTER_MODELS,
            baseline_specs: &[],
        },
        OpenAiPresetDescriptor {
            name: NVIDIA_NAME,
            display_name: NVIDIA_DISPLAY_NAME,
            endpoint: NVIDIA_ENDPOINT,
            host: "integrate.api.nvidia.com",
            extra_headers: &[],
            models: NVIDIA_MODELS,
            baseline_specs: &[],
        },
        OpenAiPresetDescriptor {
            name: OPENCODE_ZEN_NAME,
            display_name: OPENCODE_ZEN_DISPLAY_NAME,
            endpoint: OPENCODE_ZEN_ENDPOINT,
            host: "opencode.ai",
            extra_headers: &[],
            models: OPENCODE_ZEN_MODELS,
            baseline_specs: &[],
        },
        OpenAiPresetDescriptor {
            name: DEEPSEEK_NAME,
            display_name: DEEPSEEK_DISPLAY_NAME,
            endpoint: DEEPSEEK_ENDPOINT,
            host: "api.deepseek.com",
            extra_headers: &[],
            models: DEEPSEEK_MODELS,
            baseline_specs: DEEPSEEK_SPECS,
        },
        OpenAiPresetDescriptor {
            name: QWEN_NAME,
            display_name: QWEN_DISPLAY_NAME,
            endpoint: QWEN_ENDPOINT,
            host: "dashscope.aliyuncs.com",
            extra_headers: &[],
            models: QWEN_MODELS,
            baseline_specs: QWEN_SPECS,
        },
        OpenAiPresetDescriptor {
            name: ZAI_NAME,
            display_name: ZAI_DISPLAY_NAME,
            endpoint: ZAI_ENDPOINT,
            host: "api.z.ai",
            extra_headers: &[],
            models: ZAI_MODELS,
            baseline_specs: ZAI_SPECS,
        },
        OpenAiPresetDescriptor {
            name: MINIMAX_NAME,
            display_name: MINIMAX_DISPLAY_NAME,
            endpoint: MINIMAX_ENDPOINT,
            host: "api.minimax.io",
            extra_headers: &[],
            models: MINIMAX_MODELS,
            baseline_specs: MINIMAX_SPECS,
        },
        OpenAiPresetDescriptor {
            name: MOONSHOT_NAME,
            display_name: MOONSHOT_DISPLAY_NAME,
            endpoint: MOONSHOT_ENDPOINT,
            host: "api.moonshot.ai",
            extra_headers: &[],
            models: MOONSHOT_MODELS,
            baseline_specs: MOONSHOT_SPECS,
        },
        OpenAiPresetDescriptor {
            name: GROQ_NAME,
            display_name: GROQ_DISPLAY_NAME,
            endpoint: GROQ_ENDPOINT,
            host: "api.groq.com",
            extra_headers: &[],
            models: GROQ_MODELS,
            baseline_specs: GROQ_SPECS,
        },
        OpenAiPresetDescriptor {
            name: XAI_NAME,
            display_name: XAI_DISPLAY_NAME,
            endpoint: XAI_ENDPOINT,
            host: "api.x.ai",
            extra_headers: &[],
            models: XAI_MODELS,
            baseline_specs: XAI_SPECS,
        },
        OpenAiPresetDescriptor {
            name: MISTRAL_NAME,
            display_name: MISTRAL_DISPLAY_NAME,
            endpoint: MISTRAL_ENDPOINT,
            host: "api.mistral.ai",
            extra_headers: &[],
            models: MISTRAL_MODELS,
            baseline_specs: MISTRAL_SPECS,
        },
    ];

    /// Look up a curated OpenAI-compatible preset descriptor by internal name.
    #[must_use]
    pub(crate) fn find_preset(name: &str) -> Option<&'static OpenAiPresetDescriptor> {
        OPENAI_COMPAT_PRESETS
            .iter()
            .find(|preset| preset.name == name)
    }

    /// Look up a baseline capability spec for `(connection_id, model_id)`.
    #[must_use]
    pub(crate) fn find_preset_model_spec(
        connection_id: &str,
        model_id: &str,
    ) -> Option<&'static BaselineModelSpec> {
        find_preset(connection_id)?
            .baseline_specs
            .iter()
            .find(|spec| spec.model == model_id)
    }

    /// Look up a baseline context window by model ID across all presets.
    #[must_use]
    pub(crate) fn preset_model_context_limit(model_id: &str) -> Option<u64> {
        for preset in OPENAI_COMPAT_PRESETS {
            if let Some(spec) = preset.baseline_specs.iter().find(|s| s.model == model_id) {
                return Some(u64::from(spec.context_window));
            }
            if preset.models.contains(&model_id) {
                return Some(128_000);
            }
        }
        None
    }

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
