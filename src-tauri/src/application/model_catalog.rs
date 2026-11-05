//! Local model-catalog projections (provider-runtime core).
//!
//! Pure, network-free projections over the hardcoded provider/model lists
//! that stay in `infrastructure::providers::{openai,anthropic,gemini}` (the
//! single source of truth for which models may be configured, DATABASE.md
//! §7.5) and the context-window tables of
//! [`context_stats`](crate::application::context_stats). The provider
//! execution boundary itself lives in `nexora_core::provider`; this module is
//! the listing-side counterpart used by model selection, routing, and tool
//! gating (ROADMAP.md Phase 3).
//!
//! There is no live catalog fetch and no cross-provider fallback: unknown
//! providers and unlisted model IDs resolve conservatively (empty lists,
//! `false` capability flags) — never to another provider's data.

use serde::{Deserialize, Serialize};

use crate::application::context_stats::context_limit_for;
use nexora_core::provider::ModelConfig;

/// Static, locally-curated description of one model (provider-runtime core).
///
/// This is a plain domain struct: it carries the documented context window
/// plus capability flags and performs no network I/O. The hardcoded model
/// lists stay in `infrastructure::providers::{openai,anthropic,gemini}` and
/// are aggregated by `infrastructure::providers::supported_providers`; this
/// struct only projects them (plus the existing
/// `application::context_stats::context_limit_for`) into one value. There is
/// no live catalog fetch.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelInfo {
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
    /// Whether the model can stream chunks. Always `false` while the runtime
    /// is sync-only; a future streaming task flips this per provider without
    /// changing the sync path.
    pub supports_streaming: bool,
}

impl ModelInfo {
    /// Project this model's static flags into [`ModelCapabilities`].
    ///
    /// Pure projection: no network, no catalog, local-first intact.
    #[must_use]
    pub(crate) fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            tool_call: self.supports_tools,
            vision: self.supports_vision,
            json: self.supports_json,
            streaming: self.supports_streaming,
        }
    }
}

/// Pure capability projection of a [`ModelInfo`].
///
/// Computed on the local side from curated flags only — never fetched over
/// the network.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelCapabilities {
    /// The model can be invoked with tools (`tool_call` capability).
    pub tool_call: bool,
    /// The model accepts image input.
    pub vision: bool,
    /// The model accepts a structured-output mode.
    pub json: bool,
    /// The model can stream chunks.
    pub streaming: bool,
}

/// Resolve the static [`ModelInfo`] for (`provider`, `model`).
///
/// The context window comes from the existing `context_limit_for`; the
/// capability flags come from membership in the hardcoded
/// `supported_providers()` aggregation (the single source of truth for which
/// models may be configured). Listed models are tool/vision/json capable and
/// non-streaming (sync-only runtime); unlisted IDs get conservative `false`
/// flags so tool-gating drops them when tools are required. No network, no
/// fallback to another provider.
#[must_use]
pub(crate) fn model_info_for(provider: &str, model: &str) -> ModelInfo {
    let context_limit = context_limit_for(Some(provider), Some(model));
    let listed = crate::infrastructure::providers::supported_providers()
        .iter()
        .any(|entry| entry.name == provider && entry.models.iter().any(|id| id == model));
    ModelInfo {
        name: model.to_string(),
        context_limit,
        supports_tools: listed,
        supports_vision: listed,
        supports_json: listed,
        supports_streaming: false,
    }
}

/// List the static [`ModelInfo`] values for `provider` in display order.
///
/// Returns an empty vector for an unknown provider — never a fallback to
/// another provider's models.
#[must_use]
pub(crate) fn models_for_provider(provider: &str) -> Vec<ModelInfo> {
    crate::infrastructure::providers::supported_providers()
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
///
/// Pure local filter over [`ModelInfo::supports_tools`]; used when a request
/// carries tools so a non-`tool_call` model is never offered for it.
#[must_use]
pub(crate) fn tool_capable_models(models: &[ModelInfo]) -> Vec<ModelInfo> {
    models
        .iter()
        .filter(|info| info.supports_tools)
        .cloned()
        .collect()
}

/// List model names for `provider` in display order.
///
/// When `require_tools` is true, non-`tool_call` models are filtered out
/// (local analog of a recommended-models projection: text + tool-calling
/// only). When false, every listed model is returned. Unknown providers yield
/// an empty vector — no cross-provider fallback, no network.
#[must_use]
pub(crate) fn recommended_models(provider: &str, require_tools: bool) -> Vec<String> {
    let infos = models_for_provider(provider);
    let kept: Vec<ModelInfo> = if require_tools {
        tool_capable_models(&infos)
    } else {
        infos
    };
    kept.into_iter().map(|info| info.name).collect()
}

/// Fill a missing [`ModelConfig::context_limit`] from the canonical local
/// source: the existing `context_limit_for(provider, model)`.
///
/// An explicit limit is never overwritten. Temperature and `max_tokens`
/// have no canonical local source, so they are left untouched.
#[must_use]
pub(crate) fn with_canonical_limits(config: ModelConfig, provider: &str) -> ModelConfig {
    if config.context_limit.is_some() {
        return config;
    }
    let limit = context_limit_for(Some(provider), Some(config.model_name.as_str()));
    ModelConfig {
        context_limit: Some(limit),
        ..config
    }
}

/// Effective context window of `config`: the explicit override when set,
/// otherwise the canonical local value for (`provider`,
/// [`ModelConfig::model_name`]).
#[must_use]
pub(crate) fn effective_context_limit(config: &ModelConfig, provider: &str) -> u64 {
    config
        .context_limit
        .unwrap_or_else(|| context_limit_for(Some(provider), Some(config.model_name.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::providers::openai::{COMPAT_NAME, PROVIDER_NAME};

    #[test]
    fn model_config_canonical_defaulting_fills_missing_limit() {
        let provider = "openai";
        let model = "gpt-5.6-terra";
        let config = with_canonical_limits(ModelConfig::new(model), provider);
        assert_eq!(
            config.context_limit,
            Some(context_limit_for(Some(provider), Some(model)))
        );
        assert_eq!(config.model_name, model);
        // No canonical source for temperature / max_tokens: untouched.
        assert_eq!(config.temperature, None);
        assert_eq!(config.max_tokens, None);
    }

    #[test]
    fn model_config_explicit_limit_is_preserved() {
        let config = with_canonical_limits(
            ModelConfig::new("gpt-5.6-terra").with_context_limit(Some(11_000)),
            "openai",
        );
        assert_eq!(config.context_limit, Some(11_000));
    }

    #[test]
    fn model_config_effective_limit_matches_canonical() {
        let config = ModelConfig::new("claude-sonnet-5");
        assert_eq!(
            effective_context_limit(&config, "anthropic"),
            context_limit_for(Some("anthropic"), Some("claude-sonnet-5"))
        );
        let explicit = ModelConfig::new("claude-sonnet-5").with_context_limit(Some(7_000));
        assert_eq!(effective_context_limit(&explicit, "anthropic"), 7_000);
    }

    #[test]
    fn model_info_for_listed_model_is_tool_capable_without_streaming() {
        // Query the hardcoded aggregation instead of duplicating IDs: the
        // first listed model of the first non-empty provider must resolve to
        // a fully-capable (except streaming) entry.
        let providers = crate::infrastructure::providers::supported_providers();
        let entry = providers
            .iter()
            .find(|entry| !entry.models.is_empty())
            .expect("at least one provider lists models");
        let info = model_info_for(entry.name.as_str(), entry.models[0].as_str());
        assert_eq!(info.name, entry.models[0]);
        assert!(info.context_limit > 0);
        assert!(info.supports_tools);
        assert!(info.supports_vision);
        assert!(info.supports_json);
        // Sync-only runtime: streaming stays false until a streaming task lands.
        assert!(!info.supports_streaming);
    }

    #[test]
    fn model_info_for_unknown_model_is_conservative() {
        let info = model_info_for("openai", "no-such-model-xyz");
        assert_eq!(info.name, "no-such-model-xyz");
        assert!(info.context_limit > 0);
        assert!(!info.supports_tools);
        assert!(!info.supports_vision);
        assert!(!info.supports_json);
        assert!(!info.supports_streaming);
    }

    #[test]
    fn model_capabilities_project_toolcall_flag() {
        let info = ModelInfo {
            name: "m".to_string(),
            context_limit: 8_000,
            supports_tools: true,
            supports_vision: false,
            supports_json: true,
            supports_streaming: false,
        };
        let caps = info.capabilities();
        assert!(caps.tool_call);
        assert!(!caps.vision);
        assert!(caps.json);
        assert!(!caps.streaming);
    }

    #[test]
    fn tool_capable_models_filters_synthetic_mix() {
        let capable = ModelInfo {
            name: "capable".to_string(),
            context_limit: 8_000,
            supports_tools: true,
            supports_vision: true,
            supports_json: true,
            supports_streaming: false,
        };
        let legacy = ModelInfo {
            name: "legacy".to_string(),
            context_limit: 8_000,
            supports_tools: false,
            supports_vision: false,
            supports_json: false,
            supports_streaming: false,
        };
        let filtered = tool_capable_models(&[capable.clone(), legacy]);
        assert_eq!(filtered, vec![capable]);
        assert!(tool_capable_models(&[]).is_empty());
    }

    #[test]
    fn tool_capable_models_keeps_only_tool_call_models() {
        let models = vec![
            tool_test_info("tool-model", true),
            tool_test_info("text-only-model", false),
        ];
        let kept = tool_capable_models(&models);
        assert_eq!(
            kept.iter()
                .map(|info| info.name.as_str())
                .collect::<Vec<_>>(),
            vec!["tool-model"]
        );
    }

    fn tool_test_info(name: &str, supports_tools: bool) -> ModelInfo {
        ModelInfo {
            name: name.to_string(),
            context_limit: 128_000,
            supports_tools,
            supports_vision: true,
            supports_json: true,
            supports_streaming: false,
        }
    }

    #[test]
    fn recommended_models_gates_tools_without_cross_provider_fallback() {
        // Unknown provider: empty in both modes, never another provider's list.
        assert!(recommended_models("ghost", false).is_empty());
        assert!(recommended_models("ghost", true).is_empty());
        // Known provider: plain mode lists every hardcoded ID in order...
        let all = recommended_models("openai", false);
        assert!(!all.is_empty());
        assert_eq!(
            all,
            models_for_provider("openai")
                .iter()
                .map(|info| info.name.clone())
                .collect::<Vec<_>>()
        );
        // ...while tools-required mode keeps only `tool_call` models (today
        // every listed ID is tool-capable, so the subset equals the full
        // list; the synthetic test above proves non-tool IDs are dropped).
        let gated = recommended_models("openai", true);
        assert_eq!(gated, all);
        for name in &gated {
            assert!(model_info_for("openai", name).supports_tools);
        }
    }

    #[test]
    fn models_for_provider_unknown_is_empty_no_fallback() {
        assert!(models_for_provider("ghost").is_empty());
    }

    #[test]
    fn recommended_models_gates_on_tool_capability() {
        // Listed provider models are tool-capable: requiring tools keeps the
        // whole shortlist (local analog of the recommended-models
        // projection: text + tool-calling only).
        let all = recommended_models(PROVIDER_NAME, false);
        let gated = recommended_models(PROVIDER_NAME, true);
        assert!(!gated.is_empty());
        assert_eq!(all, gated);
        // A non-tool (unlisted) model never survives the tools-required
        // projection: unlisted IDs are conservative (`supports_tools: false`)
        // so tool-gating drops them.
        assert!(!model_info_for(PROVIDER_NAME, "not-a-listed-model").supports_tools);
        assert!(
            !tool_capable_models(&[model_info_for(PROVIDER_NAME, "not-a-listed-model")])
                .iter()
                .any(|info| info.name == "not-a-listed-model")
        );
        // The user-configured endpoint has no hardcoded shortlist: both
        // projections are empty (its model comes from endpoint configuration,
        // and its tool support from the endpoint toggle).
        assert!(models_for_provider(COMPAT_NAME).is_empty());
        assert!(recommended_models(COMPAT_NAME, false).is_empty());
        assert!(recommended_models(COMPAT_NAME, true).is_empty());
    }

    #[test]
    fn unknown_provider_never_falls_back_to_another_provider() {
        // No-fallback pin: an unknown name yields empty model projections —
        // it never silently tries another provider's models.
        assert!(models_for_provider("ghost-provider").is_empty());
        assert!(recommended_models("ghost-provider", false).is_empty());
        assert!(recommended_models("ghost-provider", true).is_empty());
    }
}
