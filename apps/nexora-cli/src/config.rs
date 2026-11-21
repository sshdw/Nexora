//! `nexora config get/set <key>` over the shared allow-list and store.
//!
//! The rule half of the write gate (closed allowlist, value domains, rejection
//! taxonomy) lives in `nexora_core::config`; the provider/model facts injected
//! into it come from `nexora_core::catalog`. This module only performs the
//! CLI-specific steps: reject unknown keys loudly, never echo a rejected
//! value, and persist through the shared `app_settings` repository. Credentials
//! are unreachable here — credential-shaped keys are rejected before any
//! store access (brief §14, §18; issue #162).

use nexora_core::config::{classify_key, is_secret_adjacent_key, validate_setting, ConfigKey};

use crate::error::{from_rejection, redact_value, unknown_key, CliError};
use crate::store;

/// Verify that `key` is a key the CLI can read or write.
///
/// `validate_setting` accepts `None` for *any* key (clearing is harmless), so
/// reads must apply the allowlist check themselves. Flag keys additionally
/// require a registered flag name.
fn check_known(key: &str) -> Result<(), CliError> {
    if is_secret_adjacent_key(key) {
        return Err(unknown_key(key));
    }
    match classify_key(key) {
        None => Err(unknown_key(key)),
        Some(ConfigKey::Flag) => {
            let name = nexora_core::config::strip_flag_prefix(key).unwrap_or_default();
            if nexora_core::catalog::is_known_flag(name) {
                Ok(())
            } else {
                Err(unknown_key(key))
            }
        }
        Some(_) => Ok(()),
    }
}

/// Read one setting and render its plain-text output.
pub(crate) fn get(key: &str) -> Result<String, CliError> {
    check_known(key)?;
    match store::read(key)? {
        Some(value) => Ok(redact_value(&value)),
        None => Ok("<unset>".to_string()),
    }
}

/// Validate and persist one setting, returning a plain-text confirmation.
pub(crate) fn set(key: &str, value: &str) -> Result<String, CliError> {
    // The shared gate rejects credential-shaped and unknown keys *before* the
    // value domain is consulted, so no secret can reach the store.
    validate_setting(
        &nexora_core::catalog::cli_config_registry(),
        key,
        Some(value),
    )
    .map_err(|rejection| from_rejection(&rejection))?;
    // `validate_setting` accepts `None` (clearing) for unknown keys; `set`
    // always carries a value, but keep the explicit allowlist guard so a
    // future gate change cannot silently widen this surface.
    check_known(key)?;
    store::write(key, value)?;
    Ok(format!("set {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_secret_keys_are_rejected_before_store_access() {
        for key in ["appearance.mode", "provider.api_key", "sk-live-secret-123"] {
            assert!(check_known(key).is_err(), "'{key}' must be rejected");
        }
        assert!(check_known("appearance.theme").is_ok());
        assert!(check_known("flags.snapshots").is_ok());
        assert!(check_known("flags.ghost").is_err());
    }

    #[test]
    fn rejection_diagnostics_never_echo_a_secret_value() {
        let rejection = validate_setting(
            &nexora_core::catalog::cli_config_registry(),
            "appearance.theme",
            Some("sk-live-secret-12345"),
        )
        .expect_err("invalid theme");
        let diagnostic = from_rejection(&rejection);
        assert!(!diagnostic.message.contains("sk-live-secret-12345"));
        assert!(!diagnostic.message.contains("sk-"));
        assert_eq!(
            diagnostic.exit_code(),
            nexora_core::error::CliExitCode::Usage
        );
    }

    #[test]
    fn hostile_key_names_are_redacted_in_diagnostics() {
        let err = unknown_key("sk-live-secret-12345");
        assert!(!err.message.contains("sk-live-secret-12345"));
        assert!(err.message.contains("[redacted]"));
    }
}
