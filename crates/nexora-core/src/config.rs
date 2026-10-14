//! Allow-listed configuration keys + validation.
//!
//! Extracted from the desktop settings write boundary
//! (`src-tauri/src/commands/settings.rs` and the setup-import mirror
//! `src-tauri/src/application/import.rs`, issue #160) so the CLI and Desktop
//! accept, reject, and *say the same thing* about a setting.
//!
//! ## The allowlist is closed
//!
//! Nexora's `app_settings` table is a generic key/value store, which means the
//! write gate is the only thing standing between a caller and an arbitrary
//! persisted row. [`ConfigKey`] enumerates every writable key. Anything else
//! is rejected loudly by [`validate_setting`] — there is no pass-through, no
//! "unknown but harmless" path, and no forward-compatible prefix matching that
//! could later widen the surface by accident.
//!
//! ## Secrets are not configuration
//!
//! Provider credentials live in the OS keyring only and are *never* writable
//! through this gate. A key that looks credential-shaped — `api_key`,
//! `secret`, `token`, `password`, `credential` — is rejected as
//! [`RejectionReason::SecretAdjacentKey`] regardless of any other property.
//! That check runs *before* the allowlist match, so no future registry entry
//! can accidentally reintroduce a credential-shaped key: the guard is on the
//! name shape, not on membership.
//!
//! ## Registry-dependent domains stay with their owner
//!
//! Some value domains are defined by *this build*: which provider names exist,
//! which models are listed, whether a routing profile or MCP server list
//! parses. Those are business rules and deliberately did **not** move. The
//! desktop passes them in through [`ConfigRegistry`]; the crate owns the
//! key-shape rules, the registry-free value domains, and the exact rejection
//! messages.
//!
//! ## Rejection messages are pinned
//!
//! [`Rejection::message`] reproduces the desktop's existing strings byte for
//! byte, so extracting this module changes nothing a user can observe. See
//! [`Rejection::message`].

use crate::error::CoreErrorKind;

/// Settings key holding the appearance theme.
pub const APPEARANCE_THEME_KEY: &str = "appearance.theme";
/// Settings key holding the selected provider name.
pub const SELECTED_PROVIDER_KEY: &str = "provider.selected";
/// Settings key holding the selected model identifier.
pub const SELECTED_MODEL_KEY: &str = "provider.model";
/// Settings key holding the agent autonomy mode.
pub const AUTONOMY_KEY: &str = "agent.autonomy";
/// Settings key holding the per-run spend guard, in micro-USD.
pub const SPEND_LIMIT_KEY: &str = "agent.spend_limit_micro_usd";
/// Settings key holding the approval-park deadline, in seconds.
pub const APPROVAL_TIMEOUT_KEY: &str = "agent.approval_timeout_secs";
/// Settings key holding the chosen workspace root.
pub const WORKSPACE_ROOT_KEY: &str = "agent.workspace_root";
/// Settings key holding the recent-workspace ring (JSON array).
pub const WORKSPACE_RECENT_KEY: &str = "agent.workspace_recent";
/// Settings key holding the workspace-root registry (JSON array).
pub const WORKSPACE_ROOTS_KEY: &str = "agent.workspace_roots";
/// Settings key holding the chat routing profile (JSON array).
pub const CHAT_PROFILE_KEY: &str = "routing.profile.chat";
/// Settings key holding the agent routing profile (JSON array).
pub const AGENT_PROFILE_KEY: &str = "routing.profile.agent";
/// Settings key holding the MCP server list (JSON array).
pub const MCP_SERVERS_KEY: &str = "mcp.servers";

/// Prefix of the app-global feature-flag setting keys (`flags.<name>`).
pub const FLAG_KEY_PREFIX: &str = "flags.";

/// Theme values the current implementation defines (no others exist).
pub const VALID_THEMES: [&str; 2] = ["dark", "light"];

/// Autonomy modes the current implementation defines.
pub const VALID_AUTONOMY: [&str; 3] = ["supervised", "semi_autonomous", "full_autonomous"];

/// Longest workspace root accepted (mirrors the `conversations.workspace_root`
/// `CHECK (length <= 1024)` bound).
pub const WORKSPACE_ROOT_MAX_LEN: usize = 1024;

/// Maximum number of entries in the recent-workspace ring.
pub const WORKSPACE_RECENT_MAX: usize = 5;

/// Lowest accepted approval-park deadline, in seconds. Below one second the
/// deadline is indistinguishable from "never park".
pub const MIN_APPROVAL_TIMEOUT_SECS: u64 = 1;

/// Highest accepted approval-park deadline, in seconds. Above a day it stops
/// being a deadline.
pub const MAX_APPROVAL_TIMEOUT_SECS: u64 = 24 * 60 * 60;

/// Longest accepted custom model identifier.
pub const MAX_MODEL_LEN: usize = 200;

/// Every writable setting key that is not a `flags.*` flag.
pub const BASE_KEYS: [&str; 12] = [
    APPEARANCE_THEME_KEY,
    SELECTED_PROVIDER_KEY,
    SELECTED_MODEL_KEY,
    AUTONOMY_KEY,
    SPEND_LIMIT_KEY,
    APPROVAL_TIMEOUT_KEY,
    WORKSPACE_ROOT_KEY,
    WORKSPACE_RECENT_KEY,
    WORKSPACE_ROOTS_KEY,
    CHAT_PROFILE_KEY,
    AGENT_PROFILE_KEY,
    MCP_SERVERS_KEY,
];

/// Substrings that mark a key as credential-shaped.
///
/// Checked case-insensitively against the whole key, so `provider.api_key`,
/// `PROVIDER_SECRET`, and `flags.token_audit` are all treated as
/// secret-adjacent. Matching is on the *key name only* — a value is never
/// scanned — so this cannot leak the value it is protecting.
const SECRET_KEY_MARKERS: [&str; 6] = [
    "api_key",
    "apikey",
    "secret",
    "token",
    "password",
    "credential",
];

/// A writable configuration key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKey {
    /// [`APPEARANCE_THEME_KEY`]
    Theme,
    /// [`SELECTED_PROVIDER_KEY`]
    SelectedProvider,
    /// [`SELECTED_MODEL_KEY`]
    SelectedModel,
    /// [`AUTONOMY_KEY`]
    Autonomy,
    /// [`SPEND_LIMIT_KEY`]
    SpendLimit,
    /// [`APPROVAL_TIMEOUT_KEY`]
    ApprovalTimeout,
    /// [`WORKSPACE_ROOT_KEY`]
    WorkspaceRoot,
    /// [`WORKSPACE_RECENT_KEY`]
    WorkspaceRecent,
    /// [`WORKSPACE_ROOTS_KEY`]
    WorkspaceRoots,
    /// [`CHAT_PROFILE_KEY`]
    ChatProfile,
    /// [`AGENT_PROFILE_KEY`]
    AgentProfile,
    /// [`MCP_SERVERS_KEY`]
    McpServers,
    /// A `flags.<name>` key.
    ///
    /// The flag *name* is not carried: it borrows from the key string being
    /// classified and so cannot live in a `Copy` enum. Use
    /// [`strip_flag_prefix`] to recover it.
    Flag,
}

impl ConfigKey {
    /// The key's string form, or `None` for [`ConfigKey::Flag`].
    ///
    /// A flag key is `flags.` + name, which cannot be returned as one
    /// `&'static str` without allocating (and leaking) on every call. Use
    /// [`flag_key`] to materialize it, or match the variant directly.
    #[must_use]
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Theme => Some(APPEARANCE_THEME_KEY),
            Self::SelectedProvider => Some(SELECTED_PROVIDER_KEY),
            Self::SelectedModel => Some(SELECTED_MODEL_KEY),
            Self::Autonomy => Some(AUTONOMY_KEY),
            Self::SpendLimit => Some(SPEND_LIMIT_KEY),
            Self::ApprovalTimeout => Some(APPROVAL_TIMEOUT_KEY),
            Self::WorkspaceRoot => Some(WORKSPACE_ROOT_KEY),
            Self::WorkspaceRecent => Some(WORKSPACE_RECENT_KEY),
            Self::WorkspaceRoots => Some(WORKSPACE_ROOTS_KEY),
            Self::ChatProfile => Some(CHAT_PROFILE_KEY),
            Self::AgentProfile => Some(AGENT_PROFILE_KEY),
            Self::McpServers => Some(MCP_SERVERS_KEY),
            Self::Flag => None,
        }
    }
}

/// Build the `flags.<name>` key string for a flag name.
#[must_use]
pub fn flag_key(name: &str) -> String {
    format!("{FLAG_KEY_PREFIX}{name}")
}

/// Extract the flag name from a `flags.<name>` key, or `None` when `key` does
/// not carry the prefix.
#[must_use]
pub fn strip_flag_prefix(key: &str) -> Option<&str> {
    key.strip_prefix(FLAG_KEY_PREFIX)
}

/// True when `key` looks credential-shaped.
///
/// Matched case-insensitively on the key name. This is the guard that keeps
/// secrets out of the configuration surface: provider credentials belong to
/// the OS keyring and are never persisted in `app_settings`.
#[must_use]
pub fn is_secret_adjacent_key(key: &str) -> bool {
    let lowered = key.to_lowercase();
    SECRET_KEY_MARKERS.iter().any(|m| lowered.contains(m))
}

/// Classify `key` against the closed allowlist.
///
/// Returns `None` for an unknown key. Flag keys resolve to
/// [`ConfigKey::Flag`] without consulting the flag registry — use this to
/// reject unknown keys, and [`ConfigRegistry::is_known_flag`] to additionally
/// reject unregistered flag names.
#[must_use]
pub fn classify_key(key: &str) -> Option<ConfigKey> {
    match key {
        APPEARANCE_THEME_KEY => Some(ConfigKey::Theme),
        SELECTED_PROVIDER_KEY => Some(ConfigKey::SelectedProvider),
        SELECTED_MODEL_KEY => Some(ConfigKey::SelectedModel),
        AUTONOMY_KEY => Some(ConfigKey::Autonomy),
        SPEND_LIMIT_KEY => Some(ConfigKey::SpendLimit),
        APPROVAL_TIMEOUT_KEY => Some(ConfigKey::ApprovalTimeout),
        WORKSPACE_ROOT_KEY => Some(ConfigKey::WorkspaceRoot),
        WORKSPACE_RECENT_KEY => Some(ConfigKey::WorkspaceRecent),
        WORKSPACE_ROOTS_KEY => Some(ConfigKey::WorkspaceRoots),
        CHAT_PROFILE_KEY => Some(ConfigKey::ChatProfile),
        AGENT_PROFILE_KEY => Some(ConfigKey::AgentProfile),
        MCP_SERVERS_KEY => Some(ConfigKey::McpServers),
        other => strip_flag_prefix(other).map(|_| ConfigKey::Flag),
    }
}

/// True when `key` is a member of the closed allowlist.
///
/// Flag keys are recognized by prefix only; see [`classify_key`].
#[must_use]
pub fn is_supported_key(key: &str) -> bool {
    classify_key(key).is_some()
}

/// Build-dependent value domains, supplied by the owning application.
///
/// These are the rules that were *not* extracted: they depend on the provider
/// registry and the routing/MCP parsers that belong to the application's
/// business logic. Desktop wires them to `supported_providers()`,
/// `RoutingProfile::from_json`, `McpServerList::from_json`, and the flag
/// registry. Tests wire them to fixed closures.
pub struct ConfigRegistry<'a> {
    /// Whether `name` is a provider this build supports.
    pub is_provider: &'a dyn Fn(&str) -> bool,
    /// Whether `model` is listed by any supported provider.
    pub is_listed_model: &'a dyn Fn(&str) -> bool,
    /// Whether `model` is a syntactically valid custom model ID.
    pub is_custom_model_id: &'a dyn Fn(&str) -> bool,
    /// Whether `raw` parses as a valid routing profile.
    pub is_routing_profile: &'a dyn Fn(&str) -> bool,
    /// Whether `raw` parses as a valid MCP server list.
    pub is_mcp_server_list: &'a dyn Fn(&str) -> bool,
    /// Whether `name` is a registered feature-flag name.
    pub is_known_flag: &'a dyn Fn(&str) -> bool,
}

/// Why a setting write was rejected.
///
/// The variants exist for auditing and for callers that want a specific
/// message. [`Rejection::message`] is the pinned, user-visible text and is
/// what the desktop surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectionReason {
    /// The key is not in the allowlist, or is a credential-shaped key.
    UnknownKey {
        /// The rejected key.
        key: String,
        /// Whether the key looked credential-shaped.
        secret_adjacent: bool,
    },
    /// The key is known but the value is outside its domain.
    InvalidValue {
        /// The rejected key.
        key: String,
        /// The rejected value.
        value: String,
    },
    /// The MCP server list was rejected without echoing the value, because MCP
    /// environment material may carry secrets.
    InvalidMcpServers {
        /// The rejected key.
        key: String,
    },
}

/// A rejected setting write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// Why the write was rejected.
    pub reason: RejectionReason,
}

impl Rejection {
    /// The user-visible message, byte-identical to the desktop's existing
    /// strings.
    ///
    /// - unknown / secret-adjacent key: `setting key '<key>' is not supported`
    /// - out-of-domain value: `value '<value>' is not a valid '<key>' setting`
    /// - MCP server list: `value for '<key>' is not a valid MCP server list`
    ///
    /// The MCP arm deliberately omits the value: an MCP server entry may
    /// carry an environment variable holding a secret, and an echo would leak
    /// it into logs, a CLI stream, or the IPC payload.
    #[must_use]
    pub fn message(&self) -> String {
        match &self.reason {
            RejectionReason::UnknownKey { key, .. } => {
                format!("setting key '{key}' is not supported")
            }
            RejectionReason::InvalidValue { key, value } => {
                format!("value '{value}' is not a valid '{key}' setting")
            }
            RejectionReason::InvalidMcpServers { key } => {
                format!("value for '{key}' is not a valid MCP server list")
            }
        }
    }

    /// The taxonomy category for this rejection.
    ///
    /// Always [`CoreErrorKind::InvalidInput`], matching the desktop gate.
    #[must_use]
    pub const fn kind(&self) -> CoreErrorKind {
        CoreErrorKind::InvalidInput
    }
}

fn unknown_key(key: &str) -> Rejection {
    Rejection {
        reason: RejectionReason::UnknownKey {
            key: key.to_string(),
            secret_adjacent: is_secret_adjacent_key(key),
        },
    }
}

fn invalid_value(key: &str, value: &str) -> Rejection {
    Rejection {
        reason: RejectionReason::InvalidValue {
            key: key.to_string(),
            value: value.to_string(),
        },
    }
}

/// Parse an app-global flag value: trimmed, case-insensitive `true` / `false`
/// only. Anything else yields [`None`] (treated as absent → the hardcoded
/// default).
#[must_use]
pub fn parse_global_bool(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Domain check for [`SELECTED_MODEL_KEY`]'s custom-ID arm: 1..=[`MAX_MODEL_LEN`]
/// chars of `A-Za-z0-9._/:-+` with no `..`.
///
/// This is the syntactic half; whether a listed model ID is acceptable is the
/// registry's business.
#[must_use]
pub fn is_valid_custom_model_id(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_MODEL_LEN || value.contains("..") {
        return false;
    }
    value
        .bytes()
        .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'/' | b':' | b'-' | b'+'))
}

/// Domain check for [`SPEND_LIMIT_KEY`]: a trimmed `u64` greater than zero.
#[must_use]
pub fn is_valid_spend_limit(value: &str) -> bool {
    matches!(value.trim().parse::<u64>(), Ok(limit) if limit > 0)
}

/// Domain check for [`APPROVAL_TIMEOUT_KEY`]: a trimmed `u64` within
/// [`MIN_APPROVAL_TIMEOUT_SECS`]..=[`MAX_APPROVAL_TIMEOUT_SECS`].
#[must_use]
pub fn is_valid_approval_timeout(value: &str) -> bool {
    matches!(
        value.trim().parse::<u64>(),
        Ok(secs) if (MIN_APPROVAL_TIMEOUT_SECS..=MAX_APPROVAL_TIMEOUT_SECS).contains(&secs)
    )
}

/// Syntactic check for [`WORKSPACE_ROOT_KEY`]: a non-empty path up to
/// [`WORKSPACE_ROOT_MAX_LEN`] chars with no null byte.
///
/// The filesystem guard (existence, drive roots, canonicalization) belongs to
/// the workspace application service and was not extracted.
#[must_use]
pub fn is_valid_workspace_root_syntax(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains('\0') && value.len() <= WORKSPACE_ROOT_MAX_LEN
}

/// Strict JSON-array-of-paths check shared by the workspace list keys.
///
/// Every entry must be non-empty and within [`WORKSPACE_ROOT_MAX_LEN`].
/// `cap` bounds the entry count for the recent ring ([`WORKSPACE_RECENT_MAX`]);
/// `None` leaves the registry unbounded, because it must never evict.
#[must_use]
pub fn is_valid_workspace_path_list(value: &str, cap: Option<usize>) -> bool {
    let Ok(list) = serde_json::from_str::<Vec<String>>(value) else {
        return false;
    };
    let within_cap = cap.is_none_or(|max| list.len() <= max);
    within_cap
        && list
            .iter()
            .all(|s| !s.trim().is_empty() && s.len() <= WORKSPACE_ROOT_MAX_LEN)
}

/// Validate one setting write against the closed allowlist and its value
/// domains.
///
/// A `None` value (clearing back to the documented default state) is always
/// accepted, for every key — including keys that are otherwise unsupported.
/// Clearing never persists an invalid value.
///
/// The order of the checks is load-bearing and matches the desktop exactly:
///
/// 1. `None` clears — accepted.
/// 2. credential-shaped keys are rejected (before the allowlist match, so no
///    registry change can widen the surface);
/// 3. unknown keys are rejected;
/// 4. the value is checked against its key's domain.
///
/// # Errors
///
/// Returns the [`Rejection`] that explains which check failed and carries the
/// exact user-facing message. Inspect [`Rejection::reason`] to distinguish an
/// unknown key from an out-of-domain value.
pub fn validate_setting(
    registry: &ConfigRegistry<'_>,
    key: &str,
    value: Option<&str>,
) -> Result<(), Rejection> {
    let Some(value) = value else {
        return Ok(());
    };
    if is_secret_adjacent_key(key) {
        return Err(unknown_key(key));
    }
    match classify_key(key) {
        Some(ConfigKey::Theme) => VALID_THEMES
            .contains(&value)
            .then_some(())
            .ok_or_else(|| invalid_value(key, value)),
        Some(ConfigKey::SelectedProvider) => (registry.is_provider)(value)
            .then_some(())
            .ok_or_else(|| invalid_value(key, value)),
        Some(ConfigKey::SelectedModel) => {
            if (registry.is_listed_model)(value) || (registry.is_custom_model_id)(value) {
                Ok(())
            } else {
                Err(invalid_value(key, value))
            }
        }
        Some(ConfigKey::Autonomy) => VALID_AUTONOMY
            .contains(&value)
            .then_some(())
            .ok_or_else(|| invalid_value(key, value)),
        Some(ConfigKey::SpendLimit) => is_valid_spend_limit(value)
            .then_some(())
            .ok_or_else(|| invalid_value(key, value)),
        Some(ConfigKey::ApprovalTimeout) => is_valid_approval_timeout(value)
            .then_some(())
            .ok_or_else(|| invalid_value(key, value)),
        Some(ConfigKey::WorkspaceRoot) => is_valid_workspace_root_syntax(value)
            .then_some(())
            .ok_or_else(|| invalid_value(key, value)),
        Some(ConfigKey::WorkspaceRecent) => {
            is_valid_workspace_path_list(value, Some(WORKSPACE_RECENT_MAX))
                .then_some(())
                .ok_or_else(|| invalid_value(key, value))
        }
        Some(ConfigKey::WorkspaceRoots) => is_valid_workspace_path_list(value, None)
            .then_some(())
            .ok_or_else(|| invalid_value(key, value)),
        Some(ConfigKey::ChatProfile | ConfigKey::AgentProfile) => {
            (registry.is_routing_profile)(value)
                .then_some(())
                .ok_or_else(|| invalid_value(key, value))
        }
        Some(ConfigKey::McpServers) => {
            if (registry.is_mcp_server_list)(value) {
                Ok(())
            } else {
                // Value-free: MCP environment material may carry secrets.
                Err(Rejection {
                    reason: RejectionReason::InvalidMcpServers {
                        key: key.to_string(),
                    },
                })
            }
        }
        Some(ConfigKey::Flag) => {
            let name = strip_flag_prefix(key).unwrap_or_default();
            if (registry.is_known_flag)(name) {
                if parse_global_bool(value).is_some() {
                    Ok(())
                } else {
                    Err(invalid_value(key, value))
                }
            } else {
                Err(unknown_key(key))
            }
        }
        None => Err(unknown_key(key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry that accepts everything, so tests exercise the extracted
    /// rules rather than a build's provider table.
    fn permissive_registry() -> ConfigRegistry<'static> {
        ConfigRegistry {
            is_provider: &|_| true,
            is_listed_model: &|_| true,
            is_custom_model_id: &|v| is_valid_custom_model_id(v),
            is_routing_profile: &|_| true,
            is_mcp_server_list: &|_| true,
            is_known_flag: &|_| true,
        }
    }

    fn validate(key: &str, value: Option<&str>) -> Result<(), Rejection> {
        validate_setting(&permissive_registry(), key, value)
    }

    fn assert_accepted(key: &str, value: &str) {
        assert!(
            validate(key, Some(value)).is_ok(),
            "expected '{value}' to be accepted for '{key}'"
        );
    }

    fn assert_rejected(key: &str, value: &str) {
        assert!(
            validate(key, Some(value)).is_err(),
            "expected '{value}' to be rejected for '{key}'"
        );
    }

    #[test]
    fn clearing_is_always_allowed() {
        for key in BASE_KEYS {
            assert!(validate(key, None).is_ok(), "clearing '{key}' must work");
        }
        assert!(
            validate(&flag_key("assembly"), None).is_ok(),
            "clearing a flag key must work"
        );
        assert!(
            validate("anything.else", None).is_ok(),
            "clearing an unsupported key never persists a value"
        );
    }

    #[test]
    fn every_base_key_is_recognized() {
        for key in BASE_KEYS {
            assert!(is_supported_key(key), "'{key}' must be allow-listed");
            assert_eq!(classify_key(key).and_then(ConfigKey::as_str), Some(key));
        }
        assert_eq!(
            BASE_KEYS.len(),
            12,
            "the allowlist is closed; keep it in sync"
        );
    }

    #[test]
    fn flag_keys_round_trip_through_the_prefix() {
        let key = flag_key("assembly");
        assert_eq!(key, "flags.assembly");
        assert_eq!(strip_flag_prefix(&key), Some("assembly"));
        assert_eq!(classify_key(&key), Some(ConfigKey::Flag));
        assert!(is_supported_key(&key));
    }

    #[test]
    fn unknown_keys_are_rejected_loudly() {
        for key in ["appearance.mode", "export.format", "", "appearance.theme "] {
            assert!(!is_supported_key(key), "'{key}' must not be allow-listed");
            let err = validate(key, Some("dark")).expect_err("unknown key");
            assert_eq!(err.kind(), CoreErrorKind::InvalidInput);
            assert_eq!(
                err.message(),
                format!("setting key '{key}' is not supported")
            );
        }
    }

    #[test]
    fn secret_adjacent_keys_are_never_writable() {
        for key in [
            "provider.api_key",
            "provider.apiKey",
            "PROVIDER_SECRET",
            "credentials.openai.token",
            "agent.password",
            "appearance.credential",
            "flags.apikey_cache",
        ] {
            assert!(is_secret_adjacent_key(key), "'{key}' must look secret");
            let err = validate(key, Some("value")).expect_err("secret key");
            assert_eq!(
                err.reason,
                RejectionReason::UnknownKey {
                    key: key.to_string(),
                    secret_adjacent: true,
                }
            );
            // The message never changes, so Desktop output stays identical.
            assert_eq!(
                err.message(),
                format!("setting key '{key}' is not supported")
            );
        }
    }

    #[test]
    fn no_allow_listed_key_is_secret_adjacent() {
        // Guards against a future registry entry reintroducing a credential.
        for key in BASE_KEYS {
            assert!(
                !is_secret_adjacent_key(key),
                "'{key}' is allow-listed but looks credential-shaped"
            );
        }
        for flag in ["snapshots", "self_audit", "injection", "assembly"] {
            let key = flag_key(flag);
            assert!(
                !is_secret_adjacent_key(&key),
                "'{key}' is allow-listed but looks credential-shaped"
            );
        }
    }

    #[test]
    fn theme_domain_is_closed() {
        for theme in VALID_THEMES {
            assert_accepted(APPEARANCE_THEME_KEY, theme);
        }
        for theme in ["system", "", "DARK", "Light", "high-contrast"] {
            assert_rejected(APPEARANCE_THEME_KEY, theme);
        }
    }

    #[test]
    fn autonomy_domain_is_closed() {
        for mode in VALID_AUTONOMY {
            assert_accepted(AUTONOMY_KEY, mode);
        }
        for mode in ["", "SemiAutonomous", "semi", "auto", "supervised "] {
            assert_rejected(AUTONOMY_KEY, mode);
        }
    }

    #[test]
    fn spend_limit_accepts_positive_integers_only() {
        for value in ["1", "250000", "1000000"] {
            assert_accepted(SPEND_LIMIT_KEY, value);
        }
        for value in ["0", "-5", "abc", "", "  ", "1.5"] {
            assert_rejected(SPEND_LIMIT_KEY, value);
        }
    }

    #[test]
    fn approval_timeout_accepts_only_its_bounded_range() {
        for value in ["1", "60", "600", "86400"] {
            assert_accepted(APPROVAL_TIMEOUT_KEY, value);
        }
        assert_accepted(APPROVAL_TIMEOUT_KEY, &MIN_APPROVAL_TIMEOUT_SECS.to_string());
        assert_accepted(APPROVAL_TIMEOUT_KEY, &MAX_APPROVAL_TIMEOUT_SECS.to_string());
        for value in ["0", "-5", "abc", "", "  ", "1.5", "86401"] {
            assert_rejected(APPROVAL_TIMEOUT_KEY, value);
        }
    }

    #[test]
    fn workspace_root_syntax_rejects_junk() {
        assert_accepted(WORKSPACE_ROOT_KEY, r"C:\Users\alice\work");
        assert_accepted(WORKSPACE_ROOT_KEY, "/home/alice/work");
        let overlong = "a".repeat(WORKSPACE_ROOT_MAX_LEN + 1);
        for value in ["", "   ", overlong.as_str(), "a\0b"] {
            assert_rejected(WORKSPACE_ROOT_KEY, value);
        }
    }

    #[test]
    fn workspace_recent_ring_is_capped_and_strict() {
        assert_accepted(WORKSPACE_RECENT_KEY, r#"["C:\\a"]"#);
        assert_accepted(WORKSPACE_RECENT_KEY, r#"["a","b","c","d","e"]"#);
        assert_accepted(WORKSPACE_RECENT_KEY, "[]");
        for value in [
            "not json",
            r#"["a","b","c","d","e","f"]"#,
            r#"[""]"#,
            r#"{"a":1}"#,
        ] {
            assert_rejected(WORKSPACE_RECENT_KEY, value);
        }
    }

    #[test]
    fn workspace_registry_is_uncapped_but_strict() {
        assert_accepted(WORKSPACE_ROOTS_KEY, r#"["a","b","c","d","e","f"]"#);
        assert_accepted(WORKSPACE_ROOTS_KEY, "[]");
        for value in ["not json", r#"[""]"#, r#"["  "]"#, r#"{"a":1}"#] {
            assert_rejected(WORKSPACE_ROOTS_KEY, value);
        }
        let overlong = format!(r#"["{}"]"#, "a".repeat(WORKSPACE_ROOT_MAX_LEN + 1));
        assert_rejected(WORKSPACE_ROOTS_KEY, &overlong);
    }

    #[test]
    fn custom_model_ids_reject_whitespace_and_overlong() {
        let max_len = "a".repeat(MAX_MODEL_LEN);
        let overlong = "a".repeat(MAX_MODEL_LEN + 1);
        for value in [
            "gpt-5",
            "my-custom.model:v1",
            "a",
            "vendor/model:free",
            max_len.as_str(),
        ] {
            assert!(is_valid_custom_model_id(value), "'{value}' must be valid");
        }
        for value in [
            "gpt 5",
            " gpt-5",
            "gpt-5 ",
            "gpt\t5",
            "gpt\n5",
            "../x",
            "",
            overlong.as_str(),
        ] {
            assert!(
                !is_valid_custom_model_id(value),
                "'{value}' must be invalid"
            );
        }
    }

    #[test]
    fn model_key_uses_the_registry_and_the_custom_id_fallback() {
        let only_custom = ConfigRegistry {
            is_provider: &|_| false,
            is_listed_model: &|_| false,
            is_custom_model_id: &|v| is_valid_custom_model_id(v),
            is_routing_profile: &|_| false,
            is_mcp_server_list: &|_| false,
            is_known_flag: &|_| false,
        };
        assert!(
            validate_setting(&only_custom, SELECTED_MODEL_KEY, Some("my-custom.model:v1")).is_ok()
        );
        assert!(validate_setting(&only_custom, SELECTED_MODEL_KEY, Some("gpt 5")).is_err());
    }

    #[test]
    fn provider_key_defers_to_the_registry() {
        let registry = ConfigRegistry {
            is_provider: &|name| name == "openai",
            ..permissive_registry()
        };
        assert!(validate_setting(&registry, SELECTED_PROVIDER_KEY, Some("openai")).is_ok());
        for ghost in ["", "open ai", "OpenAI", "../openai", "ghost"] {
            assert!(validate_setting(&registry, SELECTED_PROVIDER_KEY, Some(ghost)).is_err());
        }
    }

    #[test]
    fn routing_profiles_defer_to_the_registry() {
        let registry = ConfigRegistry {
            is_routing_profile: &|raw| raw == "[]" || raw.starts_with('[') && raw.len() > 2,
            ..permissive_registry()
        };
        for key in [CHAT_PROFILE_KEY, AGENT_PROFILE_KEY] {
            assert!(validate_setting(&registry, key, Some("[]")).is_ok());
            assert!(validate_setting(&registry, key, Some(r#"[{"provider":"openai"}]"#)).is_ok());
            assert!(validate_setting(&registry, key, Some("not json")).is_err());
        }
    }

    #[test]
    fn mcp_servers_rejection_never_echoes_the_value() {
        // The offending MCP entry carries a secret-looking environment value;
        // it must not survive into the message.
        let secret =
            r#"[{"name":"leaky","command":"x","env":{"GITHUB_TOKEN":"sk-live-sentinel-42"}}]"#;
        let registry = ConfigRegistry {
            is_mcp_server_list: &|_| false,
            ..permissive_registry()
        };
        let err = validate_setting(&registry, MCP_SERVERS_KEY, Some(secret))
            .expect_err("secret MCP env must be rejected");
        assert_eq!(err.kind(), CoreErrorKind::InvalidInput);
        assert_eq!(
            err.reason,
            RejectionReason::InvalidMcpServers {
                key: MCP_SERVERS_KEY.to_string()
            }
        );
        let message = err.message();
        assert_eq!(
            message,
            format!("value for '{MCP_SERVERS_KEY}' is not a valid MCP server list")
        );
        assert!(!message.contains("sk-live-sentinel-42"));
        assert!(!message.to_lowercase().contains("sk-"));
        assert!(!message.contains("leaky"));
    }

    #[test]
    fn flag_keys_require_a_registered_name_and_a_boolean() {
        let registry = ConfigRegistry {
            is_known_flag: &|name| {
                matches!(name, "snapshots" | "self_audit" | "injection" | "assembly")
            },
            ..permissive_registry()
        };
        for name in ["snapshots", "self_audit", "injection", "assembly"] {
            let key = flag_key(name);
            for value in ["true", "false", "TRUE", "  False  "] {
                assert!(
                    validate_setting(&registry, &key, Some(value)).is_ok(),
                    "flag '{key}' must accept '{value}'"
                );
            }
            for value in ["", "yes", "1", "0", "on", "sk-live-secret"] {
                assert!(
                    validate_setting(&registry, &key, Some(value)).is_err(),
                    "flag '{key}' must reject '{value}'"
                );
            }
        }
        for key in ["flags.ghost-flag", "flags.", "flags"] {
            assert!(validate_setting(&registry, key, Some("true")).is_err());
        }
    }

    #[test]
    fn global_bool_parsing_is_strict() {
        for value in ["true", "TRUE", "  True  "] {
            assert_eq!(parse_global_bool(value), Some(true), "'{value}'");
        }
        for value in ["false", "FALSE", " False "] {
            assert_eq!(parse_global_bool(value), Some(false), "'{value}'");
        }
        for value in ["", "yes", "no", "1", "0", "on", "off", "truthy", "sk-x"] {
            assert_eq!(parse_global_bool(value), None, "'{value}'");
        }
    }

    #[test]
    fn rejection_messages_are_byte_identical_to_the_desktop() {
        // Pinned strings: extracting this module must not change one
        // character of what Desktop shows a user.
        assert_eq!(
            unknown_key("appearance.mode").message(),
            "setting key 'appearance.mode' is not supported"
        );
        assert_eq!(
            invalid_value(APPEARANCE_THEME_KEY, "system").message(),
            "value 'system' is not a valid 'appearance.theme' setting"
        );
        assert_eq!(
            Rejection {
                reason: RejectionReason::InvalidMcpServers {
                    key: MCP_SERVERS_KEY.to_string()
                }
            }
            .message(),
            "value for 'mcp.servers' is not a valid MCP server list"
        );
    }
}
