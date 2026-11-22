//! Concrete provider-executor wiring (NEX-ARCH-002 / AC-101).
//!
//! This is the only place in the crate that names the concrete provider
//! executors: it builds the process-wide [`ProviderExecutorResolver`] the
//! composition root (`src-tauri/src/lib.rs` setup) installs into the
//! application execution service. The application layer resolves executors
//! exclusively through the `nexora_core::provider` seam and never imports
//! this module, so the application ↔ infrastructure provider cycle stays
//! broken in both directions.
//!
//! The registry holds no credentials and never touches the keyring: it maps
//! internal provider names to executors. Credential acquisition and
//! validation remain the responsibility of the application service. An
//! unregistered name resolves to nothing — there is deliberately no fallback
//! to an arbitrary available provider.

use std::sync::Arc;

use nexora_core::provider::{
    CompatEndpoint, CredentialSource, ExecutorRegistry, ProviderExecutorResolver, SharedExecutor,
};

use super::anthropic::{AnthropicExecutor, PROVIDER_NAME as ANTHROPIC_PROVIDER_NAME};
use super::catalog::openai::OPENAI_COMPAT_PRESETS;
use super::chatgpt_plan::{ChatGptPlanExecutor, PROVIDER_NAME as CHATGPT_PLAN_PROVIDER};
use super::credentials::CredentialStore;
use super::gemini::{GeminiExecutor, PROVIDER_NAME as GEMINI_PROVIDER_NAME};
use super::openai::{CompatConfig, OpenAiExecutor, COMPAT_NAME, PROVIDER_NAME};
use crate::async_spawner::AsyncSpawner;

/// The concrete resolver installed by the composition root: the built-in
/// provider registry plus the user-configured-endpoint executor builder.
pub(crate) struct NexoraProviderExecutors {
    registry: ExecutorRegistry,
    spawner: Arc<dyn AsyncSpawner>,
}

impl NexoraProviderExecutors {
    /// Build a resolver with every supported concrete provider registered.
    ///
    /// Native (`openai`, `anthropic`, `gemini`) and all curated presets in
    /// [`OPENAI_COMPAT_PRESETS`] are registered here under their internal
    /// provider names.
    pub(crate) fn new(spawner: Arc<dyn AsyncSpawner>) -> Self {
        let mut registry = ExecutorRegistry::new()
            .with_executor(
                PROVIDER_NAME,
                Arc::new(OpenAiExecutor::new(Arc::clone(&spawner))),
            )
            .with_executor(
                CHATGPT_PLAN_PROVIDER,
                Arc::new(ChatGptPlanExecutor::new(Arc::clone(&spawner))),
            )
            .with_executor(
                ANTHROPIC_PROVIDER_NAME,
                Arc::new(AnthropicExecutor::new(Arc::clone(&spawner))),
            )
            .with_executor(
                GEMINI_PROVIDER_NAME,
                Arc::new(GeminiExecutor::new(Arc::clone(&spawner))),
            );
        for preset in OPENAI_COMPAT_PRESETS {
            let executor = if preset.extra_headers.is_empty() {
                OpenAiExecutor::compatible(
                    preset.name,
                    preset.endpoint.to_string(),
                    Arc::clone(&spawner),
                )
            } else {
                OpenAiExecutor::compatible_with_headers(
                    preset.name,
                    preset.endpoint.to_string(),
                    preset.extra_headers,
                    Arc::clone(&spawner),
                )
            };
            registry = registry.with_executor(preset.name, Arc::new(executor));
        }
        Self { registry, spawner }
    }
}

impl ProviderExecutorResolver for NexoraProviderExecutors {
    fn resolve(&self, name: &str) -> Option<SharedExecutor> {
        self.registry.resolve(name)
    }

    fn compat_executor(&self, config: &CompatEndpoint) -> SharedExecutor {
        // The endpoint settings arrive pre-validated from the application
        // service (and are re-validated fail-closed inside the executor on
        // every send). `model` is unused by the constructor — the request's
        // own `model` field governs the wire value — so the shim leaves it
        // blank.
        let shim = CompatConfig {
            base_url: config.endpoint.clone(),
            model: String::new(),
            organization: config.organization.clone(),
            headers: config.headers.clone(),
            supports_tools: config.supports_tools,
        };
        Arc::new(OpenAiExecutor::compatible_with_config(
            COMPAT_NAME,
            &shim,
            Arc::clone(&self.spawner),
        ))
    }
}

/// The concrete provider-executor resolver as a trait object (production
/// registry). Used by seams that only need name resolution (e.g. the e2e
/// smoke path); the composition root uses [`execution_sources`] to install
/// the full pair.
pub(crate) fn provider_executors(
    spawner: Arc<dyn AsyncSpawner>,
) -> Arc<dyn ProviderExecutorResolver> {
    Arc::new(NexoraProviderExecutors::new(spawner))
}

/// Build the process-wide execution sources for the composition root: the
/// concrete provider-executor resolver and the OS keyring credential source.
///
/// Returned as trait objects so the caller (`lib.rs` setup) installs them
/// into the application layer without the application ever naming a concrete
/// provider or the keyring store.
pub fn execution_sources(
    spawner: Arc<dyn AsyncSpawner>,
) -> (Arc<dyn ProviderExecutorResolver>, Arc<dyn CredentialSource>) {
    (provider_executors(spawner), Arc::new(CredentialStore))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_spawner::test_spawner;

    fn registry() -> Arc<dyn ProviderExecutorResolver> {
        Arc::new(NexoraProviderExecutors::new(test_spawner()))
    }

    #[test]
    fn registry_resolves_openai() {
        // The OpenAI executor is registered under the internal `openai` name.
        assert!(registry().resolve(PROVIDER_NAME).is_some());
    }

    #[test]
    fn registry_resolves_anthropic() {
        // The Anthropic executor is registered under the internal `anthropic`
        // name, independently of the OpenAI registration.
        assert!(registry().resolve(ANTHROPIC_PROVIDER_NAME).is_some());
    }

    #[test]
    fn registry_resolves_gemini() {
        // The Gemini executor is registered under the internal `gemini` name,
        // independently of the OpenAI and Anthropic registrations.
        assert!(registry().resolve(GEMINI_PROVIDER_NAME).is_some());
    }

    #[test]
    fn registry_resolves_xkiro() {
        assert!(registry().resolve("xkiro").is_some());
    }

    #[test]
    fn registry_resolves_openrouter() {
        assert!(registry().resolve("openrouter").is_some());
    }

    #[test]
    fn registry_resolves_nvidia() {
        assert!(registry().resolve("nvidia").is_some());
    }

    #[test]
    fn registry_resolves_opencode_zen() {
        assert!(registry().resolve("opencode_zen").is_some());
    }

    #[test]
    fn registry_compatible_providers_do_not_shadow_openai() {
        let executors = registry();
        assert!(executors.resolve("openai").is_some());
        assert!(executors.resolve("xkiro").is_some());
        assert!(executors.resolve("openrouter").is_some());
        assert!(executors.resolve("nvidia").is_some());
        assert!(executors.resolve("opencode_zen").is_some());
        assert!(executors.resolve("not-a-provider").is_none());
    }

    #[test]
    fn registry_does_not_resolve_unknown_provider() {
        // No executor is registered for an unregistered name; there is no
        // fallback to an arbitrary available provider.
        assert!(registry().resolve("not-a-provider").is_none());
    }

    #[test]
    fn unknown_provider_never_falls_back_to_another_provider() {
        // No-fallback pin: an unknown name resolves to `None` — it never
        // silently tries another provider's executor (the catalog-side half
        // of this pin lives in the application model-catalog module).
        assert!(registry().resolve("ghost-provider").is_none());
    }

    #[test]
    fn registry_resolves_all_eight_new_presets() {
        let r = registry();
        for name in [
            "deepseek", "qwen", "zai", "minimax", "moonshot", "groq", "xai", "mistral",
        ] {
            assert!(r.resolve(name).is_some(), "preset '{name}' must resolve");
        }
        for preset in OPENAI_COMPAT_PRESETS {
            assert!(
                r.resolve(preset.name).is_some(),
                "catalog preset '{}' must be registered",
                preset.name
            );
        }
    }

    #[test]
    fn registry_keeps_external_cli_adapters_unavailable() {
        let r = registry();
        assert!(r.resolve("cli_codex").is_none());
        assert!(r.resolve("cli_claude").is_none());
    }
}
