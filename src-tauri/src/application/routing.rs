//! Model routing profiles: ordered provider→model preferences with
//! capability-aware resolution (WS-A.5).
//!
//! A routing profile is an explicit, user-ordered list of
//! ([`RouteEntry`]) candidates persisted through the existing generic
//! settings store (FR-012) — no new tables. One profile exists per task
//! ([`TaskKind`]: chat vs agent), each under its own settings key.
//!
//! Resolution ([`resolve_route`]) walks the profile in order and returns the
//! first entry whose model passes the existing `recommended_models` gating
//! for the task (`require_tools` aware). There is deliberately NO
//! cross-provider fallback beyond the explicit profile order: a profile that
//! lists two providers is explicit user intent, not implicit fallback, so an
//! entry for an unknown provider (or a model that fails gating) is skipped
//! and resolution continues down the list — other providers' model lists are
//! never consulted to substitute a candidate.
//!
//! # Security
//!
//! Profiles carry provider/model identifiers only. Credentials stay in the OS
//! keyring (ARCHITECTURE.md §12); only presence booleans ever cross IPC, and
//! [`RoutingError`] messages use fixed category text so even adversarial
//! input can never be echoed into an error.

use serde::{Deserialize, Serialize};

use crate::application::settings::SettingsService;
use crate::infrastructure::database::{Database, DatabaseError};

/// Settings key holding the chat routing profile (JSON array of entries).
pub(crate) const CHAT_PROFILE_KEY: &str = "routing.profile.chat";

/// Settings key holding the agent routing profile (JSON array of entries).
pub(crate) const AGENT_PROFILE_KEY: &str = "routing.profile.agent";

/// Maximum entries kept in one routing profile; longer payloads are rejected.
pub(crate) const MAX_PROFILE_ENTRIES: usize = 16;

/// Maximum length of a provider name inside a profile entry.
pub(crate) const MAX_PROVIDER_LEN: usize = 64;

/// Maximum length of a model identifier inside a profile entry.
pub(crate) const MAX_MODEL_LEN: usize = 200;

/// Task a routing profile serves; each task has its own stored profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskKind {
    /// Interactive chat turns (text-first; tools optional).
    Chat,
    /// Agent runs (tool calling required).
    Agent,
}

/// Settings key holding the profile for `task`.
#[must_use]
pub(crate) fn profile_key(task: TaskKind) -> &'static str {
    match task {
        TaskKind::Chat => CHAT_PROFILE_KEY,
        TaskKind::Agent => AGENT_PROFILE_KEY,
    }
}

/// One ordered routing candidate: a preferred provider and model pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RouteEntry {
    /// Internal provider name (DATABASE.md §7.5).
    pub provider: String,
    /// Model identifier requested on that provider.
    pub model: String,
}

/// An explicit, user-ordered list of routing candidates.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct RoutingProfile {
    /// Candidates in preference order; [`resolve_route`] tries them head to
    /// tail and stops at the first one that passes gating.
    pub entries: Vec<RouteEntry>,
}

impl RoutingProfile {
    /// Built-in default for `task`, derived from the build's
    /// `supported_providers()` registry (the single source of truth, so the
    /// UI never invents providers or models).
    ///
    /// Chat defaults to the first provider's first model; agent defaults to
    /// that provider's first tool-capable model (falling back to the first
    /// model when the provider lists no tool-capable one, so the default
    /// still resolves text-first rather than vanishing).
    #[must_use]
    pub(crate) fn default_for(task: TaskKind) -> Self {
        // The match keeps the per-task selection point explicit so
        // chat/agent can diverge further without changing call sites.
        let require_tools = matches!(task, TaskKind::Agent);
        let registry = crate::infrastructure::providers::supported_providers();
        let Some(first) = registry.first() else {
            return Self::default();
        };
        let model = if require_tools {
            crate::application::execution::recommended_models(&first.name, true)
                .into_iter()
                .next()
                .or_else(|| first.models.first().cloned())
                .unwrap_or_default()
        } else {
            first.models.first().cloned().unwrap_or_default()
        };
        Self {
            entries: vec![RouteEntry {
                provider: first.name.clone(),
                model,
            }],
        }
    }

    /// Parse a stored JSON array into a profile, rejecting malformed or
    /// out-of-domain payloads with fixed, secret-free reasons.
    ///
    /// # Errors
    ///
    /// Returns [`RoutingError::InvalidProfile`] when the payload is not a
    /// JSON array of well-formed entries.
    pub(crate) fn from_json(raw: &str) -> Result<Self, RoutingError> {
        let entries: Vec<RouteEntry> =
            serde_json::from_str(raw).map_err(|_| RoutingError::InvalidProfile {
                reason: "routing profile is not a JSON array of provider/model entries",
            })?;
        let profile = Self { entries };
        profile.validate()?;
        Ok(profile)
    }

    /// Serialize a profile for the settings store.
    ///
    /// # Errors
    ///
    /// Returns [`RoutingError::InvalidProfile`] when the profile violates the
    /// structural bounds, or [`RoutingError::Serialization`] when JSON
    /// encoding fails.
    pub(crate) fn to_json(&self) -> Result<String, RoutingError> {
        self.validate()?;
        serde_json::to_string(&self.entries).map_err(|_| RoutingError::Serialization)
    }

    /// Check structural bounds: a non-empty entry list within the count cap,
    /// identifiers within length limits, a build-supported provider per
    /// entry, and a model that is either listed for that provider or a valid
    /// custom model ID.
    ///
    /// This is the single source of truth for profile validity: both the
    /// settings-command gate and the service load/save paths funnel through
    /// it (via [`Self::from_json`] / [`Self::to_json`]), so they agree
    /// exactly. Capability gating beyond listed-or-custom membership stays at
    /// resolution time ([`resolve_route`]).
    pub(crate) fn validate(&self) -> Result<(), RoutingError> {
        if self.entries.is_empty() {
            return Err(RoutingError::InvalidProfile {
                reason: "routing profile lists no entries",
            });
        }
        if self.entries.len() > MAX_PROFILE_ENTRIES {
            return Err(RoutingError::InvalidProfile {
                reason: "routing profile lists too many entries",
            });
        }
        let registry = crate::infrastructure::providers::supported_providers();
        for entry in &self.entries {
            if entry.provider.is_empty()
                || entry.provider.len() > MAX_PROVIDER_LEN
                || entry.model.is_empty()
                || entry.model.len() > MAX_MODEL_LEN
            {
                return Err(RoutingError::InvalidProfile {
                    reason: "routing profile entry has an invalid provider or model",
                });
            }
            let Some(known) = registry.iter().find(|known| known.name == entry.provider) else {
                return Err(RoutingError::InvalidProfile {
                    reason: "routing profile entry names an unsupported provider",
                });
            };
            if !known.models.iter().any(|listed| listed == &entry.model)
                && !is_valid_custom_model_id(&entry.model)
            {
                return Err(RoutingError::InvalidProfile {
                    reason: "routing profile entry names a model outside the provider's models",
                });
            }
        }
        Ok(())
    }
}

/// Custom model IDs accepted alongside the listed shortlist IDs: length
/// 1..=200 bytes, charset `A-Za-z0-9._/:-+`, no whitespace/controls, and no
/// `..` parent-traversal segment.
///
/// Single definition shared by the routing-profile validation above and the
/// settings-command gate for `provider.model`, so both paths classify custom
/// IDs identically.
pub(crate) fn is_valid_custom_model_id(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_MODEL_LEN {
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

/// Resolve the first profile entry whose model passes the existing
/// `recommended_models` gating for its provider.
///
/// When `require_tools` is set (agent tasks), only tool-capable models pass;
/// otherwise every listed model passes. Entries naming an unknown provider or
/// a model outside that provider's gated list are skipped in order. An empty
/// profile — or one where every entry is skipped — resolves to [`None`];
/// resolution never substitutes another provider's models for a skipped entry
/// (no implicit cross-provider fallback).
#[must_use]
pub(crate) fn resolve_route(entries: &[RouteEntry], require_tools: bool) -> Option<&RouteEntry> {
    entries.iter().find(|entry| {
        crate::application::execution::recommended_models(&entry.provider, require_tools)
            .iter()
            .any(|gated| gated == &entry.model)
    })
}

/// Application-layer service persisting routing profiles through the existing
/// generic settings store (FR-012). No new tables; values are JSON arrays of
/// [`RouteEntry`].
pub(crate) struct RoutingService<'a> {
    settings: SettingsService<'a>,
}

impl<'a> RoutingService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            settings: SettingsService::new(db),
        }
    }

    /// Load the profile for `task`; an absent setting resolves to the
    /// built-in [`RoutingProfile::default_for`].
    ///
    /// # Errors
    ///
    /// Returns [`RoutingError::Database`] on a failed read, or
    /// [`RoutingError::InvalidProfile`] when a stored value is corrupt (a
    /// corrupt stored value is reported, never silently replaced).
    pub(crate) fn load(&self, task: TaskKind) -> Result<RoutingProfile, RoutingError> {
        match self.settings.read(profile_key(task))? {
            None => Ok(RoutingProfile::default_for(task)),
            Some(raw) => RoutingProfile::from_json(&raw),
        }
    }

    /// Persist `profile` for `task`, validating it first.
    ///
    /// # Errors
    ///
    /// Returns [`RoutingError::InvalidProfile`] when the profile violates the
    /// structural bounds, or [`RoutingError::Database`] when the write fails.
    pub(crate) fn save(
        &self,
        task: TaskKind,
        profile: &RoutingProfile,
    ) -> Result<(), RoutingError> {
        let raw = profile.to_json()?;
        Ok(self.settings.write(profile_key(task), Some(&raw))?)
    }
}

/// Errors raised by routing-profile parsing, validation, and persistence.
///
/// Messages are fixed category text: they never echo the offending payload,
/// so formatting a [`RoutingError`] cannot leak a value that was mistakenly
/// stored under a routing key (ARCHITECTURE.md §9, §11).
#[derive(Debug)]
pub(crate) enum RoutingError {
    /// A stored or supplied profile failed structural validation.
    InvalidProfile {
        /// Fixed, secret-free reason category.
        reason: &'static str,
    },
    /// A valid profile could not be JSON-encoded for storage.
    Serialization,
    /// A settings-store read or write failed.
    Database(DatabaseError),
}

impl std::fmt::Display for RoutingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProfile { reason } => write!(f, "invalid routing profile: {reason}"),
            Self::Serialization => write!(f, "routing profile could not be encoded for storage"),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for RoutingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidProfile { .. } | Self::Serialization => None,
            Self::Database(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for RoutingError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn entry(provider: &str, model: &str) -> RouteEntry {
        RouteEntry {
            provider: provider.to_string(),
            model: model.to_string(),
        }
    }

    /// First model listed for `provider` in this build's registry.
    fn first_listed(provider: &str) -> String {
        crate::infrastructure::providers::supported_providers()
            .iter()
            .find(|known| known.name == provider)
            .expect("test provider must be registered")
            .models
            .first()
            .expect("test provider must list a model")
            .clone()
    }

    /// Open an in-memory database carrying the production `app_settings`
    /// table shape so the settings-backed service reads and writes end to
    /// end.
    fn test_db() -> Database {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE app_settings (
                key TEXT PRIMARY KEY CHECK (length(key) > 0 AND length(key) <= 200),
                value TEXT CHECK (length(value) <= 10000)
            );",
        )
        .expect("create test schema");
        Database::new(conn)
    }

    #[test]
    fn resolution_prefers_profile_head() {
        let first = first_listed("openai");
        let second = crate::infrastructure::providers::supported_providers()
            .iter()
            .find(|known| known.name == "openai")
            .expect("openai must be registered")
            .models
            .get(1)
            .expect("openai must list a second model")
            .clone();
        let entries = vec![entry("openai", &first), entry("openai", &second)];
        assert_eq!(resolve_route(&entries, false), Some(&entries[0]));
        assert_eq!(resolve_route(&entries, true), Some(&entries[0]));
    }

    #[test]
    fn resolution_skips_entries_failing_tool_gating() {
        // An unlisted model identifier never passes `recommended_models`
        // gating, so with tools required it is skipped for the next
        // profile entry; the skip is order-driven, not a provider fallback.
        let listed = first_listed("openai");
        let entries = vec![
            entry("openai", "not-a-listed-model"),
            entry("openai", &listed),
        ];
        assert_eq!(resolve_route(&entries, true), Some(&entries[1]));
    }

    #[test]
    fn resolution_without_tools_still_gates_on_listed_models() {
        // Even text-first resolution only admits listed models: an unlisted
        // head is skipped rather than passed through.
        let listed = first_listed("anthropic");
        let entries = vec![
            entry("anthropic", "not-a-listed-model"),
            entry("anthropic", &listed),
        ];
        assert_eq!(resolve_route(&entries, false), Some(&entries[1]));
    }

    #[test]
    fn explicit_order_is_not_implicit_fallback() {
        // A two-provider profile is explicit user intent: resolution walks
        // the listed order and never consults another provider's list to
        // substitute a skipped entry. Here the head names an unknown
        // provider (`recommended_models` yields nothing for it — no
        // cross-provider fallback), so resolution lands on the explicit
        // second entry, exactly as ordered.
        assert!(
            crate::application::execution::recommended_models("ghost-provider", false).is_empty()
        );
        assert!(
            crate::application::execution::recommended_models("ghost-provider", true).is_empty()
        );
        let listed = first_listed("gemini");
        let entries = vec![
            entry("openai", "not-a-listed-model"),
            entry("gemini", &listed),
        ];
        // The head fails gating for its own provider; the tail — an
        // explicitly listed second provider — resolves. Nothing outside the
        // profile order was consulted.
        assert_eq!(resolve_route(&entries, true), Some(&entries[1]));

        // And when every entry fails gating, resolution is None even though
        // other providers list plenty of valid models: no implicit fallback.
        let hopeless = vec![
            entry("openai", "not-a-listed-model"),
            entry("gemini", "also-not-listed"),
        ];
        assert_eq!(resolve_route(&hopeless, false), None);
        assert_eq!(resolve_route(&hopeless, true), None);
        assert!(resolve_route(&[], true).is_none());
    }

    #[test]
    fn defaults_come_from_the_supported_registry() {
        for task in [TaskKind::Chat, TaskKind::Agent] {
            let profile = RoutingProfile::default_for(task);
            assert_eq!(profile.entries.len(), 1);
            let head = &profile.entries[0];
            let registry = crate::infrastructure::providers::supported_providers();
            let known = registry
                .iter()
                .find(|known| known.name == head.provider)
                .expect("default provider must be registered");
            assert!(known.models.contains(&head.model));
            // Defaults resolve under both gating modes.
            assert!(resolve_route(&profile.entries, false).is_some());
            assert!(resolve_route(&profile.entries, true).is_some());
        }
    }

    #[test]
    fn agent_default_is_tool_capable_while_chat_default_is_first_listed() {
        let registry = crate::infrastructure::providers::supported_providers();
        let first = registry.first().expect("build must register a provider");
        let chat = RoutingProfile::default_for(TaskKind::Chat);
        assert_eq!(
            chat.entries,
            vec![entry(
                &first.name,
                first.models.first().cloned().unwrap_or_default().as_str()
            )],
            "chat default stays the registry head"
        );
        let agent = RoutingProfile::default_for(TaskKind::Agent);
        assert_eq!(agent.entries.len(), 1);
        let head = &agent.entries[0];
        assert_eq!(head.provider, first.name);
        // The agent default passes tool gating wherever the provider lists a
        // tool-capable model; otherwise it falls back to the first model.
        let gated = crate::application::execution::recommended_models(&head.provider, true);
        if gated.is_empty() {
            assert_eq!(
                head.model,
                first.models.first().cloned().unwrap_or_default()
            );
        } else {
            assert!(
                gated.contains(&head.model),
                "agent default {:?} must be tool-capable for {:?}",
                head.model,
                head.provider
            );
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let db = test_db();
        let service = RoutingService::new(&db);
        let profile = RoutingProfile {
            entries: vec![
                entry("openai", &first_listed("openai")),
                entry("gemini", &first_listed("gemini")),
            ],
        };
        service
            .save(TaskKind::Chat, &profile)
            .expect("save valid profile");
        assert_eq!(service.load(TaskKind::Chat).expect("load back"), profile);
        // Per-task keys are independent.
        assert_eq!(
            service.load(TaskKind::Agent).expect("agent default"),
            RoutingProfile::default_for(TaskKind::Agent)
        );
    }

    #[test]
    fn load_absent_key_resolves_to_default() {
        let db = test_db();
        let service = RoutingService::new(&db);
        assert_eq!(
            service.load(TaskKind::Chat).expect("absent reads default"),
            RoutingProfile::default_for(TaskKind::Chat)
        );
    }

    #[test]
    fn malformed_or_unknown_profiles_are_rejected_secret_free() {
        const SENTINELS: [&str; 2] = ["sk-", "top-secret-value"];
        let adversarial = r#"[{"provider":"openai","model":"sk-top-secret-value"}]"#;
        // Well-formed JSON passes parsing regardless of payload content
        // (identifiers are not secrets); malformed shapes must fail without
        // echoing the input.
        let bad_inputs = [
            "not json at all",
            "{\"entries\": []}",
            "[]",
            "[{\"provider\": \"openai\"}]",
            "[{\"provider\": \"ghost-provider\", \"model\": \"x\"}]",
            "[{\"provider\": \"\", \"model\": \"x\"}]",
            "[{\"provider\": \"openai\", \"model\": \"\"}]",
            "[{\"provider\": \"openai\", \"model\": \"gpt 5\"}]",
            "[",
        ];
        for raw in bad_inputs {
            let err = RoutingProfile::from_json(raw).expect_err("bad profile must fail");
            let rendered = format!("{err}");
            for sentinel in SENTINELS {
                assert!(
                    !rendered.to_lowercase().contains(sentinel),
                    "routing error must stay secret-free, found {sentinel:?} in {rendered:?}"
                );
            }
        }
        // The adversarial-but-well-formed payload parses (identifiers carry
        // no secrets) yet its serialization round-trips without alteration.
        let parsed = RoutingProfile::from_json(adversarial).expect("well-formed JSON parses");
        assert_eq!(parsed.entries[0].model, "sk-top-secret-value");
    }

    #[test]
    fn oversized_profiles_are_rejected() {
        let many = RoutingProfile {
            entries: (0..=MAX_PROFILE_ENTRIES)
                .map(|i| entry("openai", &format!("model-{i}")))
                .collect(),
        };
        assert!(many.to_json().is_err());
        assert!(RoutingProfile::from_json(
            &serde_json::to_string(
                &many
                    .entries
                    .iter()
                    .map(|_| entry("openai", "x"))
                    .collect::<Vec<_>>()
            )
            .expect("encode oversized")
        )
        .is_err());
    }
}
