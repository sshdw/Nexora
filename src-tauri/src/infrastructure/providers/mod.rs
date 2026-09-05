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
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_providers_lists_seven() {
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
            ]
        );
        for provider in &providers {
            assert!(
                (3..=5).contains(&provider.models.len()),
                "provider '{}' must list 3-5 models, got {}",
                provider.name,
                provider.models.len()
            );
            assert!(
                !provider.models[0].is_empty(),
                "provider '{}' default model must be non-empty",
                provider.name
            );
        }
    }
}
