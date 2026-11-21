//! AI provider integration: credential storage and provider executors
//! (ROADMAP.md Phase 3 — AI Providers; ARCHITECTURE.md §5, §7).
//!
//! The infrastructure layer is responsible for AI providers and operating
//! system integration. It exposes the OS secure keyring credential store
//! ([`credentials`], FR-014) and the concrete provider executors
//! ([`openai`], [`anthropic`], [`gemini`]) behind the provider-independent
//! `nexora_core::provider::ProviderExecutor` boundary (ARCHITECTURE.md §7;
//! NEX-ARCH-002). The supported provider definitions and hardcoded model
//! lists are aggregated here for the UI via [`supported_providers`]
//! (DATABASE.md §7.5), and [`registry`] wires the concrete executors into the
//! process-wide resolver installed by the composition root.

pub mod anthropic;
pub mod catalog;
pub mod chatgpt_oauth;
pub mod chatgpt_plan;
pub mod credentials;
pub mod gemini;
pub mod metadata;
pub mod openai;
pub mod registry;
pub mod transport;

use serde::Serialize;

pub(crate) fn is_retired_cli_provider(provider: &str) -> bool {
    matches!(provider.trim(), "cli_codex" | "cli_claude")
}

/// A supported AI provider definition, exposed for the UI (DATABASE.md §7.5).
///
/// Non-sensitive metadata only: the internal `name`, a user-facing label, and
/// the hardcoded supported model identifiers. Credentials are never included —
/// they live exclusively in the OS secure keyring (ARCHITECTURE.md §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SupportedProvider {
    /// Internal provider name; the keyring namespace key.
    pub name: String,
    /// User-facing provider label.
    pub display_name: String,
    /// Model identifiers supported by this provider, in display order.
    pub models: Vec<String>,
}

/// Return every provider supported by this build, with its supported models.
///
/// This is the single source of truth for "which providers/models may be
/// configured" (DATABASE.md §7.5: model lists are hardcoded in the MVP). It is
/// derived from the registered concrete providers and their hardcoded model
/// sets, so the UI never invents providers or models.
pub(crate) fn supported_providers() -> Vec<SupportedProvider> {
    provider_schemas()
        .into_iter()
        .map(|(name, display_name, models)| SupportedProvider {
            name: name.to_string(),
            display_name: display_name.to_string(),
            models: models.iter().copied().map(ToString::to_string).collect(),
        })
        .collect()
}

/// Look up the estimated cost rate for (`provider`, `model`): input/output
/// micro-USD per 1M tokens from the provider's hardcoded `MODEL_RATES`
/// table (abstract units matching `RunBudget`; estimates, never live pricing).
///
/// The OpenAI-compatible providers (all presets in
/// [`catalog::openai::OPENAI_COMPAT_PRESETS`] plus `openai_compat`) ride the
/// shared OpenAI-compatible path, so they resolve through the native `OpenAI`
/// table. Unknown providers and unknown model IDs yield `None` — never a
/// guessed rate; callers fall back to their own policy default (the
/// application pricing policy).
#[must_use]
pub(crate) fn rate_for_model(provider: &str, model: &str) -> Option<(u64, u64)> {
    if provider == openai::PROVIDER_NAME
        || provider == openai::COMPAT_NAME
        || catalog::openai::find_preset(provider).is_some()
    {
        return openai::rate_for_model(model);
    }
    match provider {
        anthropic::PROVIDER_NAME => anthropic::rate_for_model(model),
        gemini::PROVIDER_NAME => gemini::rate_for_model(model),
        _ => None,
    }
}

/// Look up the estimated cost rate by model ID alone, searching every native
/// rate table in registration order.
///
/// The native model IDs are provider-unique, so one ID resolves to at most
/// one entry. Unknown IDs yield `None` — never a guessed rate. This is the
/// provider-agnostic path for billing call sites that carry only the model
/// string (e.g. `RunBudget::cost_for`).
#[must_use]
pub(crate) fn rate_for_model_id(model: &str) -> Option<(u64, u64)> {
    openai::rate_for_model(model)
        .or_else(|| anthropic::rate_for_model(model))
        .or_else(|| gemini::rate_for_model(model))
}

/// Collect `(name, display_name, supported_models)` for each registered
/// provider. Built from the native providers plus the data-driven
/// [`catalog::openai::OPENAI_COMPAT_PRESETS`] table and `openai_compat`.
fn provider_schemas() -> Vec<(&'static str, &'static str, &'static [&'static str])> {
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
    schemas.push((
        chatgpt_plan::PROVIDER_NAME,
        chatgpt_plan::PROVIDER_DISPLAY_NAME,
        &[],
    ));
    for preset in catalog::openai::OPENAI_COMPAT_PRESETS {
        schemas.push((preset.name, preset.display_name, preset.models));
    }
    // The user-configured OpenAI-compatible endpoint carries no hardcoded
    // shortlist: the model identifier comes from the endpoint
    // configuration, so the list is empty by design (the UI accepts the
    // configured model identifier for it).
    schemas.push((openai::COMPAT_NAME, openai::COMPAT_DISPLAY_NAME, &[]));
    schemas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_providers_list() {
        let providers = supported_providers();
        let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "openai",
                "anthropic",
                "gemini",
                "chatgpt_subscription",
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
                "openai_compat" | "chatgpt_subscription" => 0,
                _ => 3,
            };
            assert_eq!(
                provider.models.len(),
                expected,
                "provider '{}' must list {expected} models, got {}",
                provider.name,
                provider.models.len()
            );
            if expected == 0 {
                continue;
            }
            assert!(
                !provider.models[0].is_empty(),
                "provider '{}' default model must be non-empty",
                provider.name
            );
        }
    }

    #[test]
    fn rate_lookup_known_value_unknown_none() {
        // Known native IDs resolve to their hardcoded estimates.
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
        // The OpenAI-compatible providers ride the shared path: native
        // OpenAI IDs resolve through them too.
        assert_eq!(
            rate_for_model("openrouter", "gpt-5.6-luna"),
            Some((500_000, 2_000_000))
        );
        // Unknown models and providers yield None — never a guessed rate.
        assert_eq!(rate_for_model("openai", "mystery-model-9"), None);
        assert_eq!(rate_for_model("openai", "big-pickle"), None);
        assert_eq!(rate_for_model("openai", ""), None);
        assert_eq!(rate_for_model("nope", "gpt-5.6-terra"), None);
        assert_eq!(rate_for_model("", ""), None);

        // The provider-agnostic path agrees for native IDs and stays None
        // for everything else.
        assert_eq!(
            rate_for_model_id("claude-opus-4-8"),
            Some((15_000_000, 75_000_000))
        );
        assert_eq!(
            rate_for_model_id("gemini-3.1-flash-lite"),
            Some((300_000, 1_200_000))
        );
        assert_eq!(rate_for_model_id("test-model"), None);
        assert_eq!(rate_for_model_id("big-pickle"), None);
        assert_eq!(rate_for_model_id(""), None);
    }
}
