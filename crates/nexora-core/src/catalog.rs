//! The shared provider catalog: the single source of truth for which AI
//! providers and models this build supports (CLI stream, issue #162).
//!
//! Extracted verbatim from the desktop adapter's
//! `infrastructure/providers/catalog.rs` (and Anthropic's inline consts) so
//! the Desktop and the CLI describe the *same* provider surface from the
//! *same* data. The desktop module is now a thin `pub(crate)` re-export of
//! these definitions; there is no second registry, no second model list, and
//! no second rate table (brief §3.1, §18; `NEXORA-CLI-PLAN.md` §6 P3).
//!
//! Everything here is pure, offline data: identities, hardcoded model lists,
//! estimated rates, context windows, and the read-only projections used by
//! model selection. No HTTP, no keyring, no executor construction — those
//! remain with the adapters that own them.

// Brand names (OpenAI, Gemini, xKiro, OpenRouter, NVIDIA, OpenCode Zen) are
// product names, not code identifiers; the `doc_markdown` pedantic lint would
// otherwise demand backticks in every catalog comment.
#![allow(clippy::doc_markdown)]

/// Gemini catalog slice: provider identity, supported models, the
/// live-verified lite-window subset, and the rate table. Re-exported by
/// the `gemini` executor module.
pub mod gemini {
    /// Internal provider name (DATABASE.md В§7.5); the keyring namespace key.
    pub const PROVIDER_NAME: &str = "gemini";

    /// User-facing provider label.
    pub const PROVIDER_DISPLAY_NAME: &str = "Gemini";

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
    pub const SUPPORTED_MODELS: &[&str] = &[
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
    pub const GEMINI_LITE_MODELS: &[&str] = &[
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
    /// Unknown IDs yield `None` from the rate lookup: callers fall back to
    /// the policy default, never a guessed rate.
    pub const MODEL_RATES: &[(&str, u64, u64)] = &[
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
/// rate table, and the OpenAI-compatible endpoint identities plus the
/// user-configured endpoint identity. Re-exported by the desktop adapter's
/// `openai` executor module.
///
/// The per-preset constants mirror four self-describing field names
/// (`*_NAME`, `*_DISPLAY_NAME`, `*_ENDPOINT`, `*_MODELS`) copied verbatim from
/// the desktop catalog; documenting each of the 48 individually would repeat
/// the same description without adding information. The aggregation API
/// [`supported_providers`] and the descriptor structs carry full docs.
#[allow(missing_docs)]
pub mod openai {
    /// Internal provider name (DATABASE.md В§7.5); the keyring namespace key.
    pub const PROVIDER_NAME: &str = "openai";
    /// User-facing provider label.
    pub const PROVIDER_DISPLAY_NAME: &str = "OpenAI";

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
    pub const SUPPORTED_MODELS: &[&str] = &[
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
    /// `None` from the rate lookup: callers fall back to the policy default,
    /// never a guessed rate.
    ///
    /// The default tier (`gpt-5.6-terra`) is rated exactly at the conservative
    /// policy default, so it bills identically rated or unrated.
    pub const MODEL_RATES: &[(&str, u64, u64)] = &[
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
    pub struct BaselineModelSpec {
        /// Model identifier this specification describes.
        pub model: &'static str,
        /// Curated context window, in tokens.
        pub context_window: u32,
        /// Reasoning efforts the model advertises.
        pub supported_efforts: &'static [&'static str],
        /// Whether the model exposes a thinking/reasoning channel.
        pub supports_thinking: bool,
        /// Whether the model accepts attached files.
        pub supports_attachments: bool,
        /// Whether the model accepts tool calls.
        pub supports_tools: bool,
    }

    /// Data-driven descriptor for an OpenAI-compatible provider preset.
    #[derive(Debug, Clone, Copy)]
    pub struct OpenAiPresetDescriptor {
        /// Internal provider name (also the keyring account key).
        pub name: &'static str,
        /// User-facing provider label.
        pub display_name: &'static str,
        /// Fully-qualified Chat Completions endpoint.
        pub endpoint: &'static str,
        /// Endpoint host, used for safe-header matching.
        pub host: &'static str,
        /// Extra headers required by the endpoint (non-secret metadata).
        pub extra_headers: &'static [(&'static str, &'static str)],
        /// Hardcoded model shortlist in display order.
        pub models: &'static [&'static str],
        /// Per-model baseline capability specifications.
        pub baseline_specs: &'static [BaselineModelSpec],
    }

    /// OpenAI-compatible provider identities sharing the OpenAI executor wire shape.
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
    pub const XKIRO_NAME: &str = "xkiro";
    pub const XKIRO_DISPLAY_NAME: &str = "xKiro";
    pub const XKIRO_ENDPOINT: &str = "https://api.xkiro.com/v1/chat/completions";
    pub const XKIRO_MODELS: &[&str] = &[
        "deepseek/deepseek-v4-flash",
        "qwen/qwen3.5-omni-plus:free",
        "minimax/minimax-m3:free",
        "minimax/minimax-m2.7:free",
        "qwen/qwen3.5-plus:free",
        "qwen/qwen3.6-plus:free",
        "qwen/qwen3.7-plus:free",
        "deepseek/deepseek-v4-pro",
    ];
    pub const OPENROUTER_NAME: &str = "openrouter";
    pub const OPENROUTER_DISPLAY_NAME: &str = "OpenRouter";
    pub const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
    pub const OPENROUTER_HEADERS: &[(&str, &str)] = &[
        ("HTTP-Referer", "https://github.com/sshdw/Nexora"),
        ("X-Title", "Nexora"),
    ];
    pub const OPENROUTER_MODELS: &[&str] = &[
        "inclusionai/ling-3.0-flash-fin:free",
        "nvidia/nemotron-3.5-lightning:free",
        "nvidia/nemotron-3-super-120b-a12b:free",
        "cohere/north-mini-code:free",
        "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
        "inclusionai/ling-3.0-flash-sante:free",
        "inclusionai/ling-3.0-flash-vl:free",
        "liquid/lfm-2.5-2.6b:free",
    ];
    pub const NVIDIA_NAME: &str = "nvidia";
    pub const NVIDIA_DISPLAY_NAME: &str = "NVIDIA NIM";
    pub const NVIDIA_ENDPOINT: &str = "https://integrate.api.nvidia.com/v1/chat/completions";
    pub const NVIDIA_MODELS: &[&str] = &[
        "nvidia/nemotron-3-super-120b-a12b",
        "nvidia/nemotron-3-ultra-550b-a55b",
        "meta/llama-3.2-11b-vision-instruct",
        "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning",
        "openai/gpt-oss-20b",
    ];
    pub const OPENCODE_ZEN_NAME: &str = "opencode_zen";
    pub const OPENCODE_ZEN_DISPLAY_NAME: &str = "OpenCode Zen";
    pub const OPENCODE_ZEN_ENDPOINT: &str = "https://opencode.ai/zen/v1/chat/completions";
    pub const OPENCODE_ZEN_MODELS: &[&str] = &[
        "ling-3.0-flash-fin-free",
        "nemotron-3-ultra-free",
        "nemotron-3.5-lightning-free",
        "big-pickle",
        "mimo-v2.5-free",
    ];

    pub const DEEPSEEK_NAME: &str = "deepseek";
    pub const DEEPSEEK_DISPLAY_NAME: &str = "DeepSeek";
    pub const DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com/v1/chat/completions";
    pub const DEEPSEEK_MODELS: &[&str] = &["deepseek-chat", "deepseek-reasoner"];
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

    pub const QWEN_NAME: &str = "qwen";
    pub const QWEN_DISPLAY_NAME: &str = "Alibaba / Qwen";
    pub const QWEN_ENDPOINT: &str =
        "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions";
    pub const QWEN_MODELS: &[&str] = &["qwen-max", "qwen-plus", "qwq-plus", "qwen-vl-max"];
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

    pub const ZAI_NAME: &str = "zai";
    pub const ZAI_DISPLAY_NAME: &str = "ZAI";
    pub const ZAI_ENDPOINT: &str = "https://api.z.ai/api/paas/v4/chat/completions";
    pub const ZAI_MODELS: &[&str] = &["glm-4.5", "glm-4.5-air", "glm-4-plus"];
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

    pub const MINIMAX_NAME: &str = "minimax";
    pub const MINIMAX_DISPLAY_NAME: &str = "MiniMax";
    pub const MINIMAX_ENDPOINT: &str = "https://api.minimax.io/v1/chat/completions";
    pub const MINIMAX_MODELS: &[&str] = &["MiniMax-M1", "MiniMax-Text-01"];
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

    pub const MOONSHOT_NAME: &str = "moonshot";
    pub const MOONSHOT_DISPLAY_NAME: &str = "Moonshot / Kimi";
    pub const MOONSHOT_ENDPOINT: &str = "https://api.moonshot.ai/v1/chat/completions";
    pub const MOONSHOT_MODELS: &[&str] = &[
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

    pub const GROQ_NAME: &str = "groq";
    pub const GROQ_DISPLAY_NAME: &str = "Groq";
    pub const GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
    pub const GROQ_MODELS: &[&str] = &[
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

    pub const XAI_NAME: &str = "xai";
    pub const XAI_DISPLAY_NAME: &str = "xAI";
    pub const XAI_ENDPOINT: &str = "https://api.x.ai/v1/chat/completions";
    pub const XAI_MODELS: &[&str] = &["grok-3", "grok-3-mini", "grok-2-vision-1212"];
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

    pub const MISTRAL_NAME: &str = "mistral";
    pub const MISTRAL_DISPLAY_NAME: &str = "Mistral";
    pub const MISTRAL_ENDPOINT: &str = "https://api.mistral.ai/v1/chat/completions";
    pub const MISTRAL_MODELS: &[&str] = &[
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
    pub const OPENAI_COMPAT_PRESETS: &[OpenAiPresetDescriptor] = &[
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
    pub fn find_preset(name: &str) -> Option<&'static OpenAiPresetDescriptor> {
        OPENAI_COMPAT_PRESETS
            .iter()
            .find(|preset| preset.name == name)
    }

    /// Look up a baseline capability spec for `(connection_id, model_id)`.
    #[must_use]
    pub fn find_preset_model_spec(
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
    pub fn preset_model_context_limit(model_id: &str) -> Option<u64> {
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
    /// shared OpenAI-compatible executor wire shape in the desktop adapter.
    pub const COMPAT_NAME: &str = "openai_compat";
    /// User-facing label for the configurable endpoint.
    pub const COMPAT_DISPLAY_NAME: &str = "OpenAI Compatible";
}

/// Anthropic catalog slice: provider identity, supported models and rates.
///
/// The desktop executor module re-exports these so its existing consumer paths
/// keep compiling while this crate remains the only owner of the facts.
pub mod anthropic {
    /// Internal provider name; the keyring namespace account key.
    pub const PROVIDER_NAME: &str = "anthropic";
    /// User-facing provider label.
    pub const PROVIDER_DISPLAY_NAME: &str = "Anthropic";

    /// Anthropic models currently supported by this build, in display order.
    /// The first entry is the provider default.
    pub const SUPPORTED_MODELS: &[&str] = &[
        "claude-sonnet-5",
        "claude-haiku-4-5-20251001",
        "claude-opus-4-8",
    ];

    /// Estimated cost rates per 1M tokens, micro-USD (`(model, input, output)`).
    pub const MODEL_RATES: &[(&str, u64, u64)] = &[
        ("claude-sonnet-5", 3_000_000, 15_000_000),
        ("claude-haiku-4-5-20251001", 1_000_000, 5_000_000),
        ("claude-opus-4-8", 15_000_000, 75_000_000),
    ];
}

/// Default agent access profile (mirrors the desktop settings default).
pub const DEFAULT_ACCESS_PROFILE: &str = "standard";
/// Default agent autonomy mode (mirrors the desktop settings default).
pub const DEFAULT_AUTONOMY: &str = "semi_autonomous";

/// A supported AI provider definition (metadata only, never a credential).
///
/// This is the exact wire shape the desktop `SupportedProvider` command
/// returns, now owned by the core so the CLI and the Desktop list the same
/// facts from the same value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SupportedProvider {
    /// Internal provider name; the keyring namespace account key.
    pub name: String,
    /// User-facing provider label.
    pub display_name: String,
    /// Hardcoded supported model identifiers, in display order.
    pub models: Vec<String>,
}

/// Collect `(name, display_name, supported_models)` in desktop display order.
///
/// The ordering and contents are pinned by the parity tests: native providers
/// first, then the curated OpenAI-compatible presets, then `openai_compat`.
fn schemas() -> Vec<(&'static str, &'static str, &'static [&'static str])> {
    let mut schemas = vec![
        (
            openai::PROVIDER_NAME,
            openai::PROVIDER_DISPLAY_NAME,
            openai::SUPPORTED_MODELS,
        ),
        (
            anthropic::PROVIDER_NAME,
            anthropic::PROVIDER_DISPLAY_NAME,
            anthropic::SUPPORTED_MODELS,
        ),
        (
            gemini::PROVIDER_NAME,
            gemini::PROVIDER_DISPLAY_NAME,
            gemini::SUPPORTED_MODELS,
        ),
    ];
    for preset in openai::OPENAI_COMPAT_PRESETS {
        schemas.push((preset.name, preset.display_name, preset.models));
    }
    // The user-configured endpoint has no hardcoded shortlist by design.
    schemas.push((openai::COMPAT_NAME, openai::COMPAT_DISPLAY_NAME, &[]));
    schemas
}

/// Return every provider supported by this build, with its supported models.
///
/// Single source of truth for the provider surface (brief §3.1, §9.1): both
/// `nexora providers list` and Desktop's `supported_providers` IPC command
/// project this exact value.
#[must_use]
pub fn supported_providers() -> Vec<SupportedProvider> {
    schemas()
        .into_iter()
        .map(|(name, display_name, models)| SupportedProvider {
            name: name.to_string(),
            display_name: display_name.to_string(),
            models: models.iter().copied().map(ToString::to_string).collect(),
        })
        .collect()
}

/// Whether `name` is a provider this build supports.
#[must_use]
pub fn is_supported_provider(name: &str) -> bool {
    schemas().iter().any(|(candidate, _, _)| *candidate == name)
}

/// Whether `model` is listed by any supported provider.
#[must_use]
pub fn is_listed_model(model: &str) -> bool {
    schemas()
        .iter()
        .any(|(_, _, models)| models.contains(&model))
}

fn lookup_rate(table: &[(&str, u64, u64)], model: &str) -> Option<(u64, u64)> {
    table
        .iter()
        .find(|(name, _, _)| *name == model)
        .map(|(_, input, output)| (*input, *output))
}

/// Look up the estimated `(input, output)` rate in micro-USD per 1M tokens.
///
/// OpenAI-compatible providers ride the shared OpenAI rate table; unknown
/// providers and models yield `None` — never a guessed rate.
#[must_use]
pub fn rate_for_model(provider: &str, model: &str) -> Option<(u64, u64)> {
    if provider == openai::PROVIDER_NAME
        || provider == openai::COMPAT_NAME
        || openai::find_preset(provider).is_some()
    {
        return lookup_rate(openai::MODEL_RATES, model);
    }
    match provider {
        p if p == anthropic::PROVIDER_NAME => lookup_rate(anthropic::MODEL_RATES, model),
        p if p == gemini::PROVIDER_NAME => lookup_rate(gemini::MODEL_RATES, model),
        _ => None,
    }
}

/// Look up an estimated rate by model ID alone, searching native tables in
/// registration order. Unknown IDs yield `None` — never a guessed rate.
#[must_use]
pub fn rate_for_model_id(model: &str) -> Option<(u64, u64)> {
    lookup_rate(openai::MODEL_RATES, model)
        .or_else(|| lookup_rate(anthropic::MODEL_RATES, model))
        .or_else(|| lookup_rate(gemini::MODEL_RATES, model))
}

/// Conservative documented context window for OpenAI-hosted models.
pub const OPENAI_CONTEXT_LIMIT: u64 = 128_000;
/// Conservative documented context window for Anthropic-hosted models.
pub const ANTHROPIC_CONTEXT_LIMIT: u64 = 200_000;
/// Conservative documented context window for Gemini-hosted models.
pub const GEMINI_CONTEXT_LIMIT: u64 = 1_048_576;
/// Verified context window for the Gemini lite subset (1M tokens).
pub const GEMINI_LITE_CONTEXT_LIMIT: u64 = 1_048_576;
/// Fallback when the provider/model is unknown or has no committed window.
pub const DEFAULT_CONTEXT_LIMIT: u64 = 128_000;

/// Per-model context window derived from the catalog's model lists, so the
/// window table cannot drift from the provider surface. Preset baseline specs
/// and shortlists are consulted before the native lists; unlisted IDs return
/// `None` so [`context_limit_for`] falls through to its provider fallback.
#[must_use]
pub fn model_context_limit(model: &str) -> Option<u64> {
    if openai::SUPPORTED_MODELS.contains(&model) {
        Some(OPENAI_CONTEXT_LIMIT)
    } else if let Some(limit) = openai::preset_model_context_limit(model) {
        Some(limit)
    } else if anthropic::SUPPORTED_MODELS.contains(&model) {
        Some(ANTHROPIC_CONTEXT_LIMIT)
    } else if gemini::SUPPORTED_MODELS.contains(&model) {
        if gemini::GEMINI_LITE_MODELS.contains(&model) {
            Some(GEMINI_LITE_CONTEXT_LIMIT)
        } else {
            Some(GEMINI_CONTEXT_LIMIT)
        }
    } else {
        None
    }
}

/// Resolve the documented context limit for `provider`/`model`.
///
/// Mirrors the desktop `context_limit_for` exactly, from the same catalog
/// facts. No network, no invented per-model windows.
#[must_use]
pub fn context_limit_for(provider: Option<&str>, model: Option<&str>) -> u64 {
    if let Some(limit) = model.and_then(model_context_limit) {
        return limit;
    }
    match provider {
        Some(p) if p == anthropic::PROVIDER_NAME => ANTHROPIC_CONTEXT_LIMIT,
        Some(p) if p == gemini::PROVIDER_NAME => GEMINI_CONTEXT_LIMIT,
        Some(p) if p == openai::PROVIDER_NAME => OPENAI_CONTEXT_LIMIT,
        _ => {
            if model.is_some_and(|m| m.contains("gemini")) {
                GEMINI_CONTEXT_LIMIT
            } else {
                DEFAULT_CONTEXT_LIMIT
            }
        }
    }
}

/// Static, locally-curated description of one model (no network I/O).
///
/// The four capability flags mirror the desktop `ModelInfo` wire shape
/// one-for-one; collapsing them into an enum would change that contract.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelInfo {
    /// Model identifier as listed for its provider.
    pub name: String,
    /// Documented context window for this model.
    pub context_limit: u64,
    /// Whether the model accepts function-calling tools.
    pub supports_tools: bool,
    /// Whether the model accepts image input.
    pub supports_vision: bool,
    /// Whether the model accepts a structured-output (`json`) mode.
    pub supports_json: bool,
    /// Whether the model can stream chunks.
    pub supports_streaming: bool,
}

/// Pure capability projection of a [`ModelInfo`].
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ModelCapabilities {
    /// The model can be invoked with tools.
    pub tool_call: bool,
    /// The model accepts image input.
    pub vision: bool,
    /// The model accepts a structured-output mode.
    pub json: bool,
    /// The model can stream chunks.
    pub streaming: bool,
}

impl ModelInfo {
    /// Project this model's static flags into [`ModelCapabilities`].
    #[must_use]
    pub fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            tool_call: self.supports_tools,
            vision: self.supports_vision,
            json: self.supports_json,
            streaming: self.supports_streaming,
        }
    }
}

/// Resolve the static [`ModelInfo`] for (`provider`, `model`).
///
/// Listed models are tool/vision/json capable; unlisted IDs get conservative
/// `false` flags so tool-gating drops them. Unknown providers never fall back
/// to another provider's data.
#[must_use]
pub fn model_info_for(provider: &str, model: &str) -> ModelInfo {
    let listed = supported_providers()
        .iter()
        .any(|entry| entry.name == provider && entry.models.iter().any(|id| id == model));
    ModelInfo {
        name: model.to_string(),
        context_limit: context_limit_for(Some(provider), Some(model)),
        supports_tools: listed,
        supports_vision: listed,
        supports_json: listed,
        supports_streaming: listed,
    }
}

/// List the static [`ModelInfo`] values for `provider` in display order.
/// Unknown providers yield an empty vector — no cross-provider fallback.
#[must_use]
pub fn models_for_provider(provider: &str) -> Vec<ModelInfo> {
    supported_providers()
        .iter()
        .find(|entry| entry.name == provider)
        .map(|entry| {
            entry
                .models
                .iter()
                .map(|model| model_info_for(provider, model))
                .collect()
        })
        .unwrap_or_default()
}

/// Keep only tool-capable models.
#[must_use]
pub fn tool_capable_models(models: &[ModelInfo]) -> Vec<ModelInfo> {
    models
        .iter()
        .filter(|info| info.supports_tools)
        .cloned()
        .collect()
}

/// List model names for `provider`, optionally keeping only tool-capable ones.
#[must_use]
pub fn recommended_models(provider: &str, require_tools: bool) -> Vec<String> {
    let infos = models_for_provider(provider);
    let kept = if require_tools {
        tool_capable_models(&infos)
    } else {
        infos
    };
    kept.into_iter().map(|info| info.name).collect()
}

/// The set of registered global feature-flag names (mirrors the desktop flag
/// registry used by its settings gate).
const KNOWN_FLAGS: [&str; 4] = ["snapshots", "self_audit", "injection", "assembly"];

/// Whether `name` is a registered global feature flag.
#[must_use]
pub fn is_known_flag(name: &str) -> bool {
    KNOWN_FLAGS.contains(&name)
}

fn is_routing_profile(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    let Some(entries) = value.as_array() else {
        return false;
    };
    if entries.is_empty() || entries.len() > 16 {
        return false;
    }
    entries.iter().all(|entry| {
        let Some(object) = entry.as_object() else {
            return false;
        };
        let Some(provider) = object.get("provider").and_then(serde_json::Value::as_str) else {
            return false;
        };
        let Some(model) = object.get("model").and_then(serde_json::Value::as_str) else {
            return false;
        };
        is_supported_provider(provider)
            && (is_listed_model(model) || crate::config::is_valid_custom_model_id(model))
    })
}

/// Whether `raw` is a storable MCP server list.
///
/// The CLI applies the shared rule half from [`crate::config`] plus these
/// catalog-backed predicates; the Desktop additionally enforces round-trip
/// parsing and workspace guards. Every entry must at minimum identify a server
/// by non-empty string `name` and `command` fields — a bare array of arbitrary
/// objects (which could carry secret-bearing fields) is not storable. Full
/// byte-parity of every key is an owner-owned follow-up, not a reason to widen
/// this gate here.
fn is_mcp_server_list(raw: &str) -> bool {
    serde_json::from_str::<Vec<serde_json::Value>>(raw).is_ok_and(|entries| {
        entries.iter().all(|entry| {
            let Some(object) = entry.as_object() else {
                return false;
            };
            let has_non_empty_string = |field: &str| {
                object
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty())
            };
            has_non_empty_string("name") && has_non_empty_string("command")
        })
    })
}

/// Build the build-dependent half of [`crate::config::ConfigRegistry`] for the
/// CLI from this single catalog.
///
/// The rule half (allowlist, domains, pinned rejection messages) lives in
/// [`crate::config`]; the provider/model facts injected here come from this
/// module. The CLI applies those shared rules plus these catalog-backed
/// predicates, while the Desktop additionally enforces round-trip parsing and
/// workspace guards. A CLI `config set` and a Desktop setting write therefore
/// agree on every shared key today; full byte-parity of every key is an
/// owner-owned follow-up.
#[must_use]
pub fn cli_config_registry() -> crate::config::ConfigRegistry<'static> {
    crate::config::ConfigRegistry {
        is_provider: &is_supported_provider,
        is_listed_model: &is_listed_model,
        is_custom_model_id: &crate::config::is_valid_custom_model_id,
        is_routing_profile: &is_routing_profile,
        is_mcp_server_list: &is_mcp_server_list,
        is_known_flag: &is_known_flag,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_list_is_the_desktop_fact_order() {
        let providers = supported_providers();
        let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "openai",
                "anthropic",
                "gemini",
                "xkiro",
                "openrouter",
                "nvidia",
                "opencode_zen",
                "deepseek",
                "qwen",
                "zai",
                "minimax",
                "moonshot",
                "groq",
                "xai",
                "mistral",
                "openai_compat",
            ]
        );
        for provider in &providers {
            let expected = match provider.name.as_str() {
                "gemini" => 7,
                "xkiro" | "openrouter" => 8,
                "nvidia" | "opencode_zen" => 5,
                "deepseek" | "minimax" => 2,
                "qwen" | "moonshot" | "groq" | "mistral" => 4,
                "openai_compat" => 0,
                _ => 3,
            };
            assert_eq!(
                provider.models.len(),
                expected,
                "provider '{}' drifted from the desktop fact",
                provider.name
            );
            assert!(!provider.display_name.is_empty());
            if provider.name != "openai_compat" {
                assert!(!provider.models[0].is_empty());
            }
        }
    }

    #[test]
    fn rate_lookups_are_identical_to_the_desktop_tables() {
        assert_eq!(
            rate_for_model("openai", "gpt-5.6-terra"),
            Some((5_000_000, 25_000_000))
        );
        assert_eq!(
            rate_for_model("anthropic", "claude-haiku-4-5-20251001"),
            Some((1_000_000, 5_000_000))
        );
        assert_eq!(
            rate_for_model("gemini", "gemini-3.6-flash"),
            Some((1_250_000, 5_000_000))
        );
        // Compat providers ride the shared OpenAI rate table.
        assert_eq!(
            rate_for_model("openrouter", "gpt-5.6-luna"),
            Some((500_000, 2_000_000))
        );
        assert_eq!(rate_for_model("openai", "mystery-model-9"), None);
        assert_eq!(rate_for_model("nope", "gpt-5.6-terra"), None);
        assert_eq!(
            rate_for_model_id("claude-opus-4-8"),
            Some((15_000_000, 75_000_000))
        );
        assert_eq!(
            rate_for_model_id("gemini-3.1-flash-lite"),
            Some((300_000, 1_200_000))
        );
        assert_eq!(rate_for_model_id("test-model"), None);
    }

    #[test]
    fn context_limits_match_the_desktop_resolver() {
        assert_eq!(
            context_limit_for(Some("openai"), None),
            OPENAI_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(Some("anthropic"), None),
            ANTHROPIC_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(Some("gemini"), None),
            GEMINI_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(None, Some("nonsense")),
            DEFAULT_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(Some("gemini"), Some("gemini-3.1-flash-lite")),
            GEMINI_LITE_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(Some("openai"), Some("gpt-5.6-terra")),
            OPENAI_CONTEXT_LIMIT
        );
    }

    #[test]
    fn model_projections_are_conservative_without_fallback() {
        assert!(models_for_provider("ghost").is_empty());
        assert!(recommended_models("ghost", true).is_empty());
        let infos = models_for_provider("openai");
        assert_eq!(
            infos
                .iter()
                .map(|info| info.name.as_str())
                .collect::<Vec<_>>(),
            openai::SUPPORTED_MODELS
        );
        for info in &infos {
            // Desktop fact: every listed model gets all four capability
            // flags, including `supports_streaming`. Keep the projection
            // byte-identical rather than "fixing" it here.
            assert!(
                info.supports_tools
                    && info.supports_vision
                    && info.supports_json
                    && info.supports_streaming
            );
        }
        let unknown = model_info_for("openai", "no-such-model-xyz");
        assert!(!unknown.supports_tools);
        assert!(!unknown.capabilities().tool_call);
        assert!(models_for_provider(openai::COMPAT_NAME).is_empty());
    }

    #[test]
    fn mcp_server_predicate_requires_identity_and_command() {
        let valid = r#"[{"name":"local","command":"npx","args":["-y","server"]}]"#;
        assert!(is_mcp_server_list(valid));
        assert!(is_mcp_server_list("[]"));
        // Missing or blank identity / command must never be storable.
        assert!(!is_mcp_server_list(r#"[{"name":"local"}]"#));
        assert!(!is_mcp_server_list(r#"[{"command":"npx"}]"#));
        assert!(!is_mcp_server_list(r#"[{"name":"","command":"npx"}]"#));
        assert!(!is_mcp_server_list(r#"[{"name":"local","command":"  "}]"#));
        assert!(!is_mcp_server_list(r#"[{"name":"local","command":7}]"#));
        // Arbitrary secret-bearing objects cannot smuggle key material in.
        assert!(!is_mcp_server_list(
            r#"[{"api_key":"sk-live-secret-12345"}]"#
        ));
        assert!(!is_mcp_server_list("not-json"));
    }

    #[test]
    fn cli_registry_accepts_catalog_facts_and_rejects_ghosts() {
        let registry = cli_config_registry();
        assert!(crate::config::validate_setting(
            &registry,
            crate::config::SELECTED_PROVIDER_KEY,
            Some("openai")
        )
        .is_ok());
        assert!(crate::config::validate_setting(
            &registry,
            crate::config::SELECTED_PROVIDER_KEY,
            Some("ghost")
        )
        .is_err());
        assert!(crate::config::validate_setting(
            &registry,
            crate::config::SELECTED_MODEL_KEY,
            Some("gpt-5.6-terra")
        )
        .is_ok());
        assert!(crate::config::validate_setting(
            &registry,
            crate::config::SELECTED_MODEL_KEY,
            Some("a-custom.model:v1")
        )
        .is_ok());
    }
}
