//! `nexora models list [--provider <id>]` over the shared catalog.
//!
//! Every row comes from [`nexora_core::catalog::models_for_provider`], the
//! same projection Desktop's model picker uses. Unknown providers yield a
//! loud validation error; there is no cross-provider fallback and no network.

use std::fmt::Write as _;

use crate::error::{safe_key_label, CliError};

/// Render the model list for one provider (or every provider).
pub(crate) fn list(provider: Option<&str>) -> Result<String, CliError> {
    let providers = match provider {
        Some(name) => {
            if !nexora_core::catalog::is_supported_provider(name) {
                return Err(CliError::validation(format!(
                    "provider '{}' is not supported by this build",
                    safe_key_label(name)
                )));
            }
            vec![name.to_string()]
        }
        None => nexora_core::catalog::supported_providers()
            .into_iter()
            .map(|entry| entry.name)
            .collect(),
    };

    let mut out = String::from(
        "PROVIDER          MODEL                                     CONTEXT   TOOLS\n",
    );
    for name in providers {
        let infos = nexora_core::catalog::models_for_provider(&name);
        if infos.is_empty() {
            // The user-configured endpoint has no hardcoded shortlist by
            // design. Represent that fact explicitly instead of silently
            // dropping a catalog provider from the listing: the row carries
            // no invented model id, rate, or capability.
            let _ = writeln!(
                out,
                "{:<17} {:<44} {:<9} -",
                name, "(no curated models)", "-",
            );
            continue;
        }
        for info in infos {
            let _ = writeln!(
                out,
                "{:<17} {:<44} {:<9} {}",
                name,
                info.name,
                info.context_limit,
                if info.supports_tools { "yes" } else { "no" },
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexora_core::catalog::{model_info_for, supported_providers};

    #[test]
    fn all_providers_list_matches_the_shared_catalog() {
        let text = list(None).expect("list");
        assert!(!text.contains('\u{1b}'), "no ANSI escapes");
        for provider in supported_providers() {
            assert!(
                text.contains(&provider.name),
                "missing provider {}",
                provider.name
            );
            for model in provider.models {
                assert!(text.contains(&model), "missing model {model}");
            }
        }
    }

    #[test]
    fn filtered_list_uses_the_same_projection() {
        let text = list(Some("openai")).expect("filtered list");
        let info = model_info_for("openai", "gpt-5.6-terra");
        assert!(text.contains(&info.name));
        assert!(text.contains(&info.context_limit.to_string()));
        assert!(!text.contains("claude-sonnet-5"), "no cross-provider rows");
    }

    #[test]
    fn unknown_provider_is_loud_and_never_echoes_secret_bytes() {
        const SECRET: &str = "sk-live-sentinel-12345";
        let err = list(Some(SECRET)).expect_err("unknown provider");
        assert!(!err.message.contains(SECRET));
        assert!(err.message.contains("[redacted]"));
    }
}
