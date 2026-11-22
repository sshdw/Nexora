//! Local model-catalog projections (provider-runtime core).
//!
//! Pure, network-free projections over the shared catalog in
//! [`nexora_core::catalog`]. The hardcoded model lists, rates, context windows
//! and capability flags now have exactly one home (CLI stream, issue #162);
//! this module keeps the desktop call paths compiling unchanged by
//! re-exporting the core types and delegating every projection. There is no
//! live catalog fetch and no cross-provider fallback: unknown providers and
//! unlisted model IDs resolve conservatively, never to another provider's
//! data.

use nexora_core::provider::ModelConfig;

use crate::application::context_stats::context_limit_for;

pub(crate) use nexora_core::catalog::ModelInfo;

/// Resolve the static [`ModelInfo`] for (`provider`, `model`).
#[must_use]
pub(crate) fn model_info_for(provider: &str, model: &str) -> ModelInfo {
    nexora_core::catalog::model_info_for(provider, model)
}

/// List the static [`ModelInfo`] values for `provider` in display order.
#[must_use]
pub(crate) fn models_for_provider(provider: &str) -> Vec<ModelInfo> {
    nexora_core::catalog::models_for_provider(provider)
}

/// Keep only tool-capable models.
#[must_use]
pub(crate) fn tool_capable_models(models: &[ModelInfo]) -> Vec<ModelInfo> {
    nexora_core::catalog::tool_capable_models(models)
}

/// List model names for `provider`, optionally keeping only tool-capable ones.
#[must_use]
pub(crate) fn recommended_models(provider: &str, require_tools: bool) -> Vec<String> {
    nexora_core::catalog::recommended_models(provider, require_tools)
}

/// Fill a missing [`ModelConfig::context_limit`] from the canonical local
/// source: the existing `context_limit_for(provider, model)`.
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
/// otherwise the canonical local value for (`provider`, model).
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
        assert!(info.supports_streaming);
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
        assert!(recommended_models("ghost", false).is_empty());
        assert!(recommended_models("ghost", true).is_empty());
        let all = recommended_models("openai", false);
        assert!(!all.is_empty());
        assert_eq!(
            all,
            models_for_provider("openai")
                .iter()
                .map(|info| info.name.clone())
                .collect::<Vec<_>>()
        );
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
        let all = recommended_models(PROVIDER_NAME, false);
        let gated = recommended_models(PROVIDER_NAME, true);
        assert!(!gated.is_empty());
        assert_eq!(all, gated);
        assert!(!model_info_for(PROVIDER_NAME, "not-a-listed-model").supports_tools);
        assert!(
            !tool_capable_models(&[model_info_for(PROVIDER_NAME, "not-a-listed-model")])
                .iter()
                .any(|info| info.name == "not-a-listed-model")
        );
        assert!(models_for_provider(COMPAT_NAME).is_empty());
        assert!(recommended_models(COMPAT_NAME, false).is_empty());
        assert!(recommended_models(COMPAT_NAME, true).is_empty());
    }

    #[test]
    fn unknown_provider_never_falls_back_to_another_provider() {
        assert!(models_for_provider("ghost-provider").is_empty());
        assert!(recommended_models("ghost-provider", false).is_empty());
        assert!(recommended_models("ghost-provider", true).is_empty());
    }
}
