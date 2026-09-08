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

use crate::application::settings::SettingsService;
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
/// - Only the four explicitly supported keys may be written.
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
///
/// No credential, payload, or path value can appear here: only the four
/// keys above reach persistence, and none of them ever carries a secret.
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
        _ => Err(CommandError::new(
            ErrorKind::InvalidInput,
            format!("setting key '{key}' is not supported"),
        )),
    }
}

/// Custom model IDs accepted for [`SELECTED_MODEL_KEY`] alongside the listed
/// shortlist IDs: length 1..=200 bytes, charset `A-Za-z0-9._/:-+`, no
/// whitespace/controls, and no `..` parent-traversal segment.
fn is_valid_custom_model_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 200 {
        return false;
    }
    if value.contains("..") {
        return false;
    }
    value.bytes().all(|byte| {
        matches!(
            byte,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'.'
                | b'_'
                | b'/'
                | b':'
                | b'-'
                | b'+'
        )
    })
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
    /// valid for every key вЂ” including keys that are otherwise unsupported.
    #[test]
    fn clearing_is_always_allowed() {
        for key in [
            THEME_KEY,
            SELECTED_PROVIDER_KEY,
            SELECTED_MODEL_KEY,
            AUTONOMY_KEY,
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
        // the four explicitly supported setting keys are writable.
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
}
