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
use super::credentials::CredentialStore;
use super::gemini::{GeminiExecutor, PROVIDER_NAME as GEMINI_PROVIDER_NAME};
use super::openai::{
    CompatConfig, OpenAiExecutor, COMPAT_NAME, NVIDIA_ENDPOINT, NVIDIA_NAME, OPENCODE_ZEN_ENDPOINT,
    OPENCODE_ZEN_NAME, OPENROUTER_ENDPOINT, OPENROUTER_NAME, PROVIDER_NAME, XKIRO_ENDPOINT,
    XKIRO_NAME,
};

/// The concrete resolver installed by the composition root: the built-in
/// provider registry plus the user-configured-endpoint executor builder.
pub(crate) struct NexoraProviderExecutors {
    registry: ExecutorRegistry,
}

impl NexoraProviderExecutors {
    /// Build a resolver with every supported concrete provider registered.
    ///
    /// The `openai`, `anthropic`, and `gemini` provider executors are
    /// registered here, each under its internal provider name. Additional
    /// providers are registered here as they ship.
    pub(crate) fn new() -> Self {
        Self {
            registry: ExecutorRegistry::new()
                .with_executor(PROVIDER_NAME, Arc::new(OpenAiExecutor::new()))
                .with_executor(ANTHROPIC_PROVIDER_NAME, Arc::new(AnthropicExecutor::new()))
                .with_executor(GEMINI_PROVIDER_NAME, Arc::new(GeminiExecutor::new()))
                // Additional OpenAI-compatible providers share `OpenAiExecutor`.
                .with_executor(
                    XKIRO_NAME,
                    Arc::new(OpenAiExecutor::compatible(
                        XKIRO_NAME,
                        XKIRO_ENDPOINT.to_string(),
                    )),
                )
                .with_executor(
                    OPENROUTER_NAME,
                    Arc::new(OpenAiExecutor::compatible_with_headers(
                        OPENROUTER_NAME,
                        OPENROUTER_ENDPOINT.to_string(),
                        &[
                            ("HTTP-Referer", "https://github.com/sshdw/Nexora"),
                            ("X-Title", "Nexora"),
                        ],
                    )),
                )
                .with_executor(
                    NVIDIA_NAME,
                    Arc::new(OpenAiExecutor::compatible(
                        NVIDIA_NAME,
                        NVIDIA_ENDPOINT.to_string(),
                    )),
                )
                .with_executor(
                    OPENCODE_ZEN_NAME,
                    Arc::new(OpenAiExecutor::compatible(
                        OPENCODE_ZEN_NAME,
                        OPENCODE_ZEN_ENDPOINT.to_string(),
                    )),
                ),
        }
    }
}

impl Default for NexoraProviderExecutors {
    fn default() -> Self {
        Self::new()
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
        Arc::new(OpenAiExecutor::compatible_with_config(COMPAT_NAME, &shim))
    }
}

/// The concrete provider-executor resolver as a trait object (production
/// registry). Used by seams that only need name resolution (e.g. the e2e
/// smoke path); the composition root uses [`execution_sources`] to install
/// the full pair.
pub(crate) fn provider_executors() -> Arc<dyn ProviderExecutorResolver> {
    Arc::new(NexoraProviderExecutors::new())
}

/// Build the process-wide execution sources for the composition root: the
/// concrete provider-executor resolver and the OS keyring credential source.
///
/// Returned as trait objects so the caller (`lib.rs` setup) installs them
/// into the application layer without the application ever naming a concrete
/// provider or the keyring store.
pub(crate) fn execution_sources() -> (Arc<dyn ProviderExecutorResolver>, Arc<dyn CredentialSource>)
{
    (provider_executors(), Arc::new(CredentialStore))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Arc<dyn ProviderExecutorResolver> {
        Arc::new(NexoraProviderExecutors::new())
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
}
