//! Tauri commands over the existing [`SettingsService`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer settings service (FR-012) and converts its
//! classified errors into safe [`CommandError`] values. Settings are key/value
//! pairs; provider credentials are never stored here (ARCHITECTURE.md В§12).
//!
//! FR-012 requires that invalid values are rejected. The generic key/value
//! store therefore validates at this command boundary before any persistence
//! happens: only the explicitly supported setting keys may be written, and
//! their values must belong to the domains defined by the existing
//! implementation (themes; the build's supported providers/models). Clearing
//! a value (`None`) is always allowed вЂ” it restores the documented default
//! state and never persists an invalid value. This validation does not affect
//! `clear_application_data`, which clears through the repositories directly.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::routing::{
    is_valid_custom_model_id, RoutingProfile, AGENT_PROFILE_KEY, CHAT_PROFILE_KEY,
};
use crate::application::settings::SettingsService;
use crate::application::workspace::{
    parse_recent, WORKSPACE_RECENT_KEY, WORKSPACE_RECENT_MAX, WORKSPACE_ROOT_KEY,
    WORKSPACE_ROOT_MAX_LEN,
};
use crate::infrastructure::database::Database;
use crate::infrastructure::providers::supported_providers;

use super::error::{CommandError, ErrorKind};

/// The appearance theme key persisted by the Settings view (Phase 10.8).
pub(crate) const THEME_KEY: &str = "appearance.theme";

/// The selected-provider key persisted by the provider/model hook (FR-004).
pub(crate) const SELECTED_PROVIDER_KEY: &str = "provider.selected";

/// The selected-model key persisted by the provider/model hook (FR-004).
pub(crate) const SELECTED_MODEL_KEY: &str = "provider.model";

/// Autonomy mode persisted for agent runs (Task 5.2, DP-AUTONOMY).
pub(crate) const AUTONOMY_KEY: &str = "agent.autonomy";

/// Theme values defined by the current implementation (no others exist).
const VALID_THEMES: &[&str] = &["dark", "light"];

/// Valid autonomy modes for [`AUTONOMY_KEY`] (Task 5.2).
const VALID_AUTONOMY: &[&str] = &["supervised", "semi_autonomous", "full_autonomous"];

/// Validate one setting write against the domains defined by the existing
/// implementation (FR-012: invalid values are rejected before persistence).
///
/// Rules:
/// - Only the eight explicitly supported keys may be written.
/// - A `None` value (clearing back to the default state) is always valid.
/// - [`THEME_KEY`] accepts only the implemented themes (`dark`, `light`).
/// - [`SELECTED_PROVIDER_KEY`] accepts only names returned by the build's
///   `supported_providers()` registry (the single source of truth).
/// - [`SELECTED_MODEL_KEY`] accepts model identifiers listed for a
///   supported provider (union across providers; the writer orders model
///   before provider when switching, so per-provider coupling is not assumed)
///   or a custom model ID (1..=200 chars of `A-Za-z0-9._/:-+`, no `..`).
/// - [`AUTONOMY_KEY`] accepts only the three autonomy modes
///   (`supervised`, `semi_autonomous`, `full_autonomous`).
/// - [`CHAT_PROFILE_KEY`] / [`AGENT_PROFILE_KEY`] accept a JSON array of
///   1..=16 `{provider, model}` entries where each provider is a supported
///   provider name and each model is listed for that provider or a valid
///   custom model ID (same charset rule as [`SELECTED_MODEL_KEY`]).
///   Resolution-time `recommended_models` gating still applies at use; the
///   stored profile only records explicit user order.
/// - [`WORKSPACE_ROOT_KEY`] accepts a non-empty path up to 1024 chars with no
///   null byte. Full filesystem guard (existence, `C:\Windows`, drive roots,
///   canonicalization) lives in the workspace commands
///   (`commands/workspace.rs::set_workspace_root` via
///   `application/workspace.rs::validate_workspace_root`); this syntactic
///   check only keeps obvious junk out of the generic key/value path.
/// - [`WORKSPACE_RECENT_KEY`] accepts a JSON array of at most 5 non-empty
///   strings, each up to 1024 chars.
///
/// No credential, payload, or path value can appear here beyond the workspace
/// root strings: only the eight keys above reach persistence, and none of them
/// ever carries a secret.
fn validate_setting(key: &str, value: Option<&str>) -> Result<(), CommandError> {
    // Clearing a setting restores its default state; never an invalid value.
    let Some(value) = value else {
        return Ok(());
    };
    match key {
        THEME_KEY => {
            if VALID_THEMES.contains(&value) {
                Ok(())
            } else {
                Err(rejected(key, value))
            }
        }
        SELECTED_PROVIDER_KEY => {
            if supported_providers().iter().any(|p| p.name == value) {
                Ok(())
            } else {
                Err(rejected(key, value))
            }
        }
        SELECTED_MODEL_KEY => {
            if supported_providers()
                .iter()
                .any(|p| p.models.iter().any(|m| m == value))
                || is_valid_custom_model_id(value)
            {
                Ok(())
            } else {
                Err(rejected(key, value))
            }
        }
        AUTONOMY_KEY => {
            if VALID_AUTONOMY.contains(&value) {
                Ok(())
            } else {
                Err(rejected(key, value))
            }
        }
        WORKSPACE_ROOT_KEY => {
            if value.trim().is_empty()
                || value.contains('\0')
                || value.len() > WORKSPACE_ROOT_MAX_LEN
            {
                Err(rejected(key, value))
            } else {
                Ok(())
            }
        }
        WORKSPACE_RECENT_KEY => {
            let items = parse_recent(Some(value));
            // `parse_recent` truncates to the max; a round-trip mismatch means
            // the stored value was corrupt or overfull. Re-parse strictly:
            // the value must be a JSON array of <= 5 non-empty <= 1024 strings.
            let strict: bool = match serde_json::from_str::<Vec<String>>(value) {
                Ok(list) => {
                    list.len() <= WORKSPACE_RECENT_MAX
                        && list
                            .iter()
                            .all(|s| !s.trim().is_empty() && s.len() <= WORKSPACE_ROOT_MAX_LEN)
                        && items.len() == list.len()
                }
                Err(_) => false,
            };
            if strict {
                Ok(())
            } else {
                Err(rejected(key, value))
            }
        }
        CHAT_PROFILE_KEY | AGENT_PROFILE_KEY => {
            if is_valid_routing_profile(value) {
                Ok(())
            } else {
                Err(rejected(key, value))
            }
        }
        _ => Err(CommandError::new(
            ErrorKind::InvalidInput,
            format!("setting key '{key}' is not supported"),
        )),
    }
}

/// Routing profiles accepted for [`CHAT_PROFILE_KEY`] / [`AGENT_PROFILE_KEY`].
///
/// No independent logic lives here: validity is decided solely by
/// [`RoutingProfile::from_json`] (parse plus the single shared validation),
/// so the command gate and the service load/save paths agree exactly.
fn is_valid_routing_profile(value: &str) -> bool {
    RoutingProfile::from_json(value).is_ok()
}

/// Build the uniform secret-free rejection for an out-of-domain value.
fn rejected(key: &str, value: &str) -> CommandError {
    CommandError::new(
        ErrorKind::InvalidInput,
        format!("value '{value}' is not a valid '{key}' setting"),
    )
}

/// Read one setting by `key` (`None` when absent).
#[tauri::command]
pub(crate) fn get_setting(
    key: String,
    db: State<'_, Database>,
) -> Result<Option<String>, CommandError> {
    SettingsService::new(db.inner())
        .read(&key)
        .map_err(Into::into)
}

/// Write one setting by `key` (`value` may be `None` to store a `NULL`).
///
/// FR-012: the write is rejected with [`ErrorKind::InvalidInput`] before any
/// persistence happens unless the key/value pair belongs to the domains
/// defined by [`validate_setting`].
#[tauri::command]
pub(crate) fn set_setting(
    key: String,
    value: Option<String>,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    validate_setting(&key, value.as_deref())?;
    SettingsService::new(db.inner())
        .write(&key, value.as_deref())
        .map_err(Into::into)
}

/// Delete one setting by `key` (a no-op when it does not exist).
#[tauri::command]
pub(crate) fn delete_setting(key: String, db: State<'_, Database>) -> Result<(), CommandError> {
    SettingsService::new(db.inner())
        .delete(&key)
        .map_err(Into::into)
}

/// List every setting as `(key, value)` pairs, ordered by `key`.
#[tauri::command]
pub(crate) fn list_settings(
    db: State<'_, Database>,
) -> Result<Vec<(String, Option<String>)>, CommandError> {
    SettingsService::new(db.inner()).list().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_accepted(key: &str, value: &str) {
        assert!(
            validate_setting(key, Some(value)).is_ok(),
            "expected '{value}' to be accepted for '{key}'"
        );
    }

    fn assert_rejected(key: &str, value: &str) {
        match validate_setting(key, Some(value)) {
            Err(err) => assert_eq!(
                err.kind,
                ErrorKind::InvalidInput,
                "expected InvalidInput rejecting '{value}' for '{key}'"
            ),
            Ok(()) => panic!("expected '{value}' to be rejected for '{key}'"),
        }
    }

    /// Clearing (`None`) restores a documented default state and is always
    /// valid for every key — including keys that are otherwise unsupported.
    #[test]
    fn clearing_is_always_allowed() {
        for key in [
            THEME_KEY,
            SELECTED_PROVIDER_KEY,
            SELECTED_MODEL_KEY,
            AUTONOMY_KEY,
            CHAT_PROFILE_KEY,
            AGENT_PROFILE_KEY,
            WORKSPACE_ROOT_KEY,
            WORKSPACE_RECENT_KEY,
        ] {
            assert!(validate_setting(key, None).is_ok());
        }
        assert!(validate_setting("anything.else", None).is_ok());
    }

    #[test]
    fn valid_theme_values_are_accepted() {
        for theme in VALID_THEMES {
            assert_accepted(THEME_KEY, theme);
        }
    }

    #[test]
    fn invalid_theme_values_are_rejected() {
        for theme in ["system", "", "DARK", "Light", "high-contrast"] {
            assert_rejected(THEME_KEY, theme);
        }
    }

    #[test]
    fn valid_provider_values_are_accepted() {
        for provider in supported_providers() {
            assert_accepted(SELECTED_PROVIDER_KEY, &provider.name);
        }
    }

    #[test]
    fn invalid_provider_values_are_rejected() {
        for provider in ["", "open ai", "ollama", "OpenAI", "../openai"] {
            assert_rejected(SELECTED_PROVIDER_KEY, provider);
        }
    }

    #[test]
    fn valid_model_values_are_accepted() {
        let models: Vec<String> = supported_providers()
            .into_iter()
            .flat_map(|provider| provider.models)
            .collect();
        assert!(!models.is_empty(), "the build must define supported models");
        for model in models {
            assert_accepted(SELECTED_MODEL_KEY, &model);
        }
    }

    #[test]
    fn invalid_model_values_are_rejected() {
        let overlong = "a".repeat(201);
        for model in ["", "gpt 5", "../x", overlong.as_str()] {
            assert_rejected(SELECTED_MODEL_KEY, model);
        }
    }

    #[test]
    fn custom_model_ids_are_accepted() {
        let max_len = "a".repeat(200);
        for model in [
            "gpt-5",
            "my-custom.model:v1",
            "a",
            "vendor/model:free",
            max_len.as_str(),
        ] {
            assert_accepted(SELECTED_MODEL_KEY, model);
        }
    }

    #[test]
    fn custom_model_ids_reject_whitespace_and_overlong() {
        let overlong = "a".repeat(201);
        for model in [
            "gpt 5",
            " gpt-5",
            "gpt-5 ",
            "gpt\t5",
            "gpt\n5",
            overlong.as_str(),
        ] {
            assert_rejected(SELECTED_MODEL_KEY, model);
        }
    }

    #[test]
    fn unknown_keys_are_rejected() {
        // A provider name is not a valid value under an arbitrary key: only
        // the eight explicitly supported setting keys are writable.
        assert_rejected("appearance.mode", "dark");
        assert_rejected("export.format", "markdown");
        assert_rejected("", "dark");
    }

    #[test]
    fn valid_autonomy_values_are_accepted() {
        for mode in VALID_AUTONOMY {
            assert_accepted(AUTONOMY_KEY, mode);
        }
    }

    #[test]
    fn invalid_autonomy_values_are_rejected() {
        for mode in ["", "SemiAutonomous", "semi", "auto", "supervised "] {
            assert_rejected(AUTONOMY_KEY, mode);
        }
    }

    #[test]
    fn workspace_root_values_are_accepted_syntactically() {
        assert_accepted(WORKSPACE_ROOT_KEY, r"C:\Users\alice\work");
        assert_accepted(WORKSPACE_ROOT_KEY, "/home/alice/work");
    }

    #[test]
    fn workspace_root_junk_values_are_rejected() {
        let overlong = "a".repeat(1025);
        assert_rejected(WORKSPACE_ROOT_KEY, "");
        assert_rejected(WORKSPACE_ROOT_KEY, "   ");
        assert_rejected(WORKSPACE_ROOT_KEY, overlong.as_str());
        assert_rejected(WORKSPACE_ROOT_KEY, "a\0b");
    }

    #[test]
    fn workspace_recent_values_are_accepted() {
        assert_accepted(WORKSPACE_RECENT_KEY, r#"["C:\\a"]"#);
        assert_accepted(WORKSPACE_RECENT_KEY, r#"["a","b","c","d","e"]"#);
        assert_accepted(WORKSPACE_RECENT_KEY, "[]");
    }

    #[test]
    fn workspace_recent_overfull_or_corrupt_is_rejected() {
        assert_rejected(WORKSPACE_RECENT_KEY, "not json");
        assert_rejected(WORKSPACE_RECENT_KEY, r#"["a","b","c","d","e","f"]"#);
        assert_rejected(WORKSPACE_RECENT_KEY, r#"[""]"#);
        assert_rejected(WORKSPACE_RECENT_KEY, r#"{"a":1}"#);
    }

    #[test]
    fn routing_profiles_are_accepted_for_both_tasks() {
        // Every registered provider accepts its own first listed model, and
        // multi-provider order (explicit user intent) is preserved verbatim.
        let registry = supported_providers();
        let first = &registry[0];
        let single =
            serde_json::json!([{"provider": first.name, "model": first.models[0]}]).to_string();
        assert_accepted(CHAT_PROFILE_KEY, &single);
        assert_accepted(AGENT_PROFILE_KEY, &single);
        if registry.len() > 1 && !registry[1].models.is_empty() {
            let second = &registry[1];
            let ordered = serde_json::json!([
                {"provider": first.name, "model": first.models[0]},
                {"provider": second.name, "model": second.models[0]},
            ])
            .to_string();
            assert_accepted(CHAT_PROFILE_KEY, &ordered);
            assert_accepted(AGENT_PROFILE_KEY, &ordered);
        }
        // A valid custom model ID is accepted alongside listed IDs.
        let custom = serde_json::json!([{"provider": first.name, "model": "my-custom.model:v1"}])
            .to_string();
        assert_accepted(CHAT_PROFILE_KEY, &custom);
    }

    #[test]
    fn routing_profiles_reject_malformed_or_out_of_domain_values() {
        let overlong_model = "a".repeat(201);
        let oversized = serde_json::to_string(
            &(0..=crate::application::routing::MAX_PROFILE_ENTRIES)
                .map(|_| serde_json::json!({"provider": "openai", "model": "x"}))
                .collect::<Vec<_>>(),
        )
        .expect("encode oversized profile");
        let bad = [
            "not json".to_string(),
            "[]".to_string(),
            "{}".to_string(),
            r#"[{"provider": "openai"}]"#.to_string(),
            r#"[{"provider": "ghost-provider", "model": "x"}]"#.to_string(),
            r#"[{"provider": "", "model": "x"}]"#.to_string(),
            r#"[{"provider": "openai", "model": ""}]"#.to_string(),
            r#"[{"provider": "openai", "model": "gpt 5"}]"#.to_string(),
            format!(r#"[{{"provider": "openai", "model": "{overlong_model}"}}]"#),
            oversized,
        ];
        for value in &bad {
            assert_rejected(CHAT_PROFILE_KEY, value);
            assert_rejected(AGENT_PROFILE_KEY, value);
        }
    }

    #[test]
    fn routing_gate_agrees_with_profile_validation() {
        // The command gate delegates to `RoutingProfile::from_json`, so it
        // must accept/reject exactly the same inputs as the service path.
        // Covers the pinned cases: empty profile and invalid-model-for-
        // provider rejected on both paths, valid profiles accepted on both.
        let listed = supported_providers()
            .into_iter()
            .find(|known| known.name == "openai")
            .expect("openai must be registered")
            .models
            .into_iter()
            .next()
            .expect("openai must list a model");
        let overlong = "a".repeat(201);
        let oversized = serde_json::to_string(
            &(0..=crate::application::routing::MAX_PROFILE_ENTRIES)
                .map(|_| serde_json::json!({"provider": "openai", "model": "x"}))
                .collect::<Vec<_>>(),
        )
        .expect("encode oversized profile");
        let cases = [
            (
                serde_json::json!([{"provider": "openai", "model": listed}]).to_string(),
                true,
            ),
            (
                serde_json::json!([{"provider": "openai", "model": "my-custom.model:v1"}])
                    .to_string(),
                true,
            ),
            ("[]".to_string(), false),
            (
                r#"[{"provider": "openai", "model": "gpt 5"}]"#.to_string(),
                false,
            ),
            (
                r#"[{"provider": "ghost-provider", "model": "x"}]"#.to_string(),
                false,
            ),
            (
                format!(r#"[{{"provider": "openai", "model": "{overlong}"}}]"#),
                false,
            ),
            (oversized, false),
            ("not json".to_string(), false),
        ];
        for (raw, expected) in &cases {
            assert_eq!(
                RoutingProfile::from_json(raw).is_ok(),
                *expected,
                "service path verdict for {raw:?}"
            );
            for key in [CHAT_PROFILE_KEY, AGENT_PROFILE_KEY] {
                assert_eq!(
                    validate_setting(key, Some(raw)).is_ok(),
                    *expected,
                    "command gate verdict for {key} {raw:?}"
                );
            }
        }
    }
}
