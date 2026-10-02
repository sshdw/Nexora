//! AI provider integration: credential storage and provider executors
//! (ROADMAP.md Phase 3 — AI Providers; ARCHITECTURE.md §5, §7).
//!
//! The infrastructure layer is responsible for AI providers and operating
//! system integration. It exposes the OS secure keyring credential store
//! ([`credentials`], FR-014) and the concrete provider executors
//! ([`openai`], [`anthropic`], [`gemini`]) behind the provider-independent
//! [`ProviderExecutor`] boundary (ARCHITECTURE.md §7). The supported provider
//! definitions and hardcoded model lists are aggregated here for the UI via
//! [`supported_providers`] (DATABASE.md §7.5).

pub mod anthropic;
pub mod credentials;
pub mod gemini;
pub mod openai;
pub mod transport;

use serde::Serialize;

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
/// The OpenAI-compatible providers (`xkiro`, `openrouter`, `nvidia`,
/// `opencode_zen`, `openai_compat`) ride the shared OpenAI-compatible path,
/// so they resolve through the native `OpenAI` table. Unknown providers and
/// unknown model IDs yield `None` — never a guessed rate; callers fall back
/// to the policy default in `application::agent::pricing`.
#[must_use]
pub(crate) fn rate_for_model(provider: &str, model: &str) -> Option<(u64, u64)> {
    match provider {
        openai::PROVIDER_NAME
        | openai::XKIRO_NAME
        | openai::OPENROUTER_NAME
        | openai::NVIDIA_NAME
        | openai::OPENCODE_ZEN_NAME
        | openai::COMPAT_NAME => openai::rate_for_model(model),
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
/// provider. Kept as a small tuple helper so the list stays a single table.
fn provider_schemas() -> Vec<(&'static str, &'static str, &'static [&'static str])> {
    vec![
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
        (
            openai::XKIRO_NAME,
            openai::XKIRO_DISPLAY_NAME,
            openai::XKIRO_MODELS,
        ),
        (
            openai::OPENROUTER_NAME,
            openai::OPENROUTER_DISPLAY_NAME,
            openai::OPENROUTER_MODELS,
        ),
        (
            openai::NVIDIA_NAME,
            openai::NVIDIA_DISPLAY_NAME,
            openai::NVIDIA_MODELS,
        ),
        (
            openai::OPENCODE_ZEN_NAME,
            openai::OPENCODE_ZEN_DISPLAY_NAME,
            openai::OPENCODE_ZEN_MODELS,
        ),
        // The user-configured OpenAI-compatible endpoint carries no hardcoded
        // shortlist: the model identifier comes from the endpoint
        // configuration, so the list is empty by design (the UI accepts the
        // configured model identifier for it).
        (openai::COMPAT_NAME, openai::COMPAT_DISPLAY_NAME, &[]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_providers_lists_eight() {
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
                "openai_compat",
            ]
        );
        for provider in &providers {
            // Native OpenAI/Anthropic keep their curated 3-model lists; Gemini
            // carries the live-smoke-gated 1.2.2 keep-list (7: chat 200 keep,
            // tools 200 agent-usable, 429 stays, 404/503 dropped). The four
            // OpenAI-compatible providers carry smoke-gated shortlists
            // (xkiro 8, openrouter 8, nvidia 5, opencode_zen 5): an ID stays
            // listed iff a live POST to the provider's `chat/completions`
            // endpoint returns chat 2xx for it, and 429-only IDs stay listed.
            // The user-configured endpoint (`openai_compat`) carries no
            // hardcoded shortlist: its model identifier comes from the
            // endpoint configuration, so the list is empty by design.
            let expected = match provider.name.as_str() {
                "gemini" => 7,
                "xkiro" | "openrouter" => 8,
                "nvidia" | "opencode_zen" => 5,
                "openai_compat" => 0,
                _ => 3,
            };
            assert_eq!(
                provider.models.len(),
                expected,
                "provider '{}' must list {expected} models, got {}",
                provider.name,
                provider.models.len()
            );
            if provider.name == "openai_compat" {
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
