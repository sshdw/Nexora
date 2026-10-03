//! Model routing profiles: ordered provider→model preferences with
//! capability-aware resolution (WS-A.5).
//!
//! A routing profile is an explicit, user-ordered list of
//! ([`RouteEntry`]) candidates persisted through the existing generic
//! settings store (FR-012) — no new tables. One profile exists per task
//! ([`TaskKind`]: chat vs agent), each under its own settings key.
//!
//! Resolution ([`resolve_route`]) walks the profile in order and returns the
//! first entry usable for the task: a model passing the existing
//! `recommended_models` gating (`require_tools` aware), or — for text-first
//! tasks only — a valid custom model ID on a supported provider (unknown
//! capability passes through when tools are not needed, and fails closed
//! when they are). There is deliberately NO
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

impl TaskKind {
    /// Parse the caller-supplied task label (`"chat"` | `"agent"`).
    ///
    /// Anything else yields [`None`]; callers report the fixed-vocabulary
    /// unknown-task error without echoing the rejected label (checkpoint-label
    /// rule, see [`ResolvedProfile`]).
    #[must_use]
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "chat" => Some(Self::Chat),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// Workspace file holding the profile for `task` under `.nexora/profiles/`.
#[must_use]
pub(crate) fn profile_file_name(task: TaskKind) -> &'static str {
    match task {
        TaskKind::Chat => "chat.json",
        TaskKind::Agent => "agent.json",
    }
}

/// Where a resolved routing profile came from (precedence order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileSource {
    /// A valid workspace `.nexora/profiles/<task>.json` file.
    Workspace,
    /// The app-global settings key ([`profile_key`]).
    Global,
    /// The built-in registry default ([`RoutingProfile::default_for`]).
    Default,
}

/// Fixed-vocabulary notice returned alongside the profile when a present
/// workspace profile file fails to load and resolution falls back to the
/// global settings key (or the registry default when the global key is
/// absent); surfaced on the run's audit trail by the agent run path
/// (`service::spawn_run` via
/// [`service::note_routing_fallback`](crate::application::agent::service::note_routing_fallback)).
///
/// The notice echoes nothing: no document content, no file name, no task
/// label. This mirrors the checkpoint-label rule from the run snapshots
/// (`application/agent/snapshots.rs`): the task label is caller-chosen input
/// (like a checkpoint name), so it may appear in read-only views but never in
/// errors or returned notes — only fixed vocabulary travels there. The run
/// itself never fails for a bad workspace file; the fallback applies and the
/// note is returned alongside the profile, surfaced on the run's audit trail
/// by the agent run path
/// ([`service::note_routing_fallback`](crate::application::agent::service::note_routing_fallback)).
pub(crate) const INVALID_WORKSPACE_PROFILE_NOTICE: &str =
    "the workspace profile is invalid; using the stored settings profile";

/// A routing profile resolved through the workspace → global → default
/// precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedProfile {
    /// Where the profile came from.
    pub source: ProfileSource,
    /// The winning profile.
    pub profile: RoutingProfile,
    /// Set to [`INVALID_WORKSPACE_PROFILE_NOTICE`] when a present workspace
    /// file failed to load and the fallback applied; [`None`] otherwise.
    pub notice: Option<&'static str>,
}

impl ResolvedProfile {
    /// First profile entry usable for the task, via [`resolve_route`].
    ///
    /// This wires precedence resolution into the existing routing call path:
    /// callers resolve once, then route through the same gating primitive.
    #[must_use]
    pub(crate) fn route(&self, require_tools: bool) -> Option<&RouteEntry> {
        resolve_route(&self.profile.entries, require_tools)
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
    /// that provider's first tool-capable model.
    ///
    /// Limitation: when the default provider lists no tool-capable model,
    /// the agent default falls back to the first listed model, which
    /// [`resolve_route`] then skips under `require_tools` (resolving to
    /// [`None`]) rather than substituting another provider. The fallback arm
    /// is defensive: `default_provider_lists_a_tool_capable_model` pins that
    /// the registry head always lists one, so the arm is dead code in
    /// practice. If that test ever fails, the defaults need an explicit
    /// redesign — do not silently pick another provider here.
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

/// Resolve the first profile entry usable for the task.
///
/// An entry passes when its model is in the existing `recommended_models`
/// gating for its provider (`require_tools` aware). Additionally, a valid
/// custom model ID (#67) on a build-supported provider passes through when
/// tools are NOT required (chat): its capability is unknown but tools are not
/// needed, and pass-through is the only resolution path for providers with
/// no hardcoded list (notably the user-configured `openai_compat` endpoint,
/// whose list is empty by design). When tools ARE required (agent), custom
/// IDs are skipped — unknown capability fails closed and never auto-runs
/// tools. Entries naming an unknown provider never pass in either mode.
///
/// Skipped entries continue down the profile in order. An empty profile — or
/// one where every entry is skipped — resolves to [`None`]; resolution never
/// substitutes another provider's models for a skipped entry (no implicit
/// cross-provider fallback).
#[must_use]
pub(crate) fn resolve_route(entries: &[RouteEntry], require_tools: bool) -> Option<&RouteEntry> {
    entries.iter().find(|entry| {
        if crate::application::execution::recommended_models(&entry.provider, require_tools)
            .iter()
            .any(|gated| gated == &entry.model)
        {
            return true;
        }
        !require_tools
            && is_valid_custom_model_id(&entry.model)
            && crate::infrastructure::providers::supported_providers()
                .iter()
                .any(|known| known.name == entry.provider)
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
    /// Unchanged global-only behavior: workspace files are consulted only
    /// through [`Self::resolve`].
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

    /// Resolve the profile for `task` through the workspace → global →
    /// default precedence.
    ///
    /// `workspace_root` is the canonical workspace root when the caller runs
    /// inside one ([`None`] skips the workspace tier): a present
    /// `.nexora/profiles/<task>.json` file that validates wins
    /// ([`ProfileSource::Workspace`]); a present file that fails to load falls
    /// back to the global settings key with the fixed-vocabulary
    /// [`INVALID_WORKSPACE_PROFILE_NOTICE`] returned alongside the profile
    /// (never failing the run, never echoing content; surfaced on the run's
    /// audit trail by the agent run path — see
    /// [`service::note_routing_fallback`](crate::application::agent::service::note_routing_fallback)); an absent file reads
    /// the global key unchanged
    /// ([`ProfileSource::Global`]); an absent global key resolves to the
    /// registry default ([`ProfileSource::Default`]).
    ///
    /// A corrupt *global* value keeps [`Self::load`] semantics: it is
    /// reported as [`RoutingError::InvalidProfile`], never silently replaced.
    ///
    /// # Errors
    ///
    /// Returns [`RoutingError::Database`] on a failed settings read, or
    /// [`RoutingError::InvalidProfile`] when a stored global value is corrupt.
    pub(crate) fn resolve(
        &self,
        workspace_root: Option<&std::path::Path>,
        task: TaskKind,
    ) -> Result<ResolvedProfile, RoutingError> {
        if let Some(root) = workspace_root {
            let file_name = profile_file_name(task);
            if crate::application::project_dir::profile_file_present(root, file_name) {
                if let Ok(profile) =
                    crate::application::project_dir::load_profile_file(root, file_name)
                {
                    return Ok(ResolvedProfile {
                        source: ProfileSource::Workspace,
                        profile,
                        notice: None,
                    });
                }
                // A present file that fails to load (invalid content,
                // oversized document, unreadable file, escaping link):
                // fall back with the fixed-vocab note, never fail the
                // run and never echo content.
                let (source, profile) = self.global_tier(task)?;
                return Ok(ResolvedProfile {
                    source,
                    profile,
                    notice: Some(INVALID_WORKSPACE_PROFILE_NOTICE),
                });
            }
            // Absent file (or unusable root): the global tier decides, with no
            // notice — there is nothing to report.
        }
        let (source, profile) = self.global_tier(task)?;
        Ok(ResolvedProfile {
            source,
            profile,
            notice: None,
        })
    }

    /// Global tier of [`Self::resolve`], read once: ([`ProfileSource::Global`],
    /// stored profile) when the settings key is present,
    /// ([`ProfileSource::Default`], registry default) when absent.
    fn global_tier(&self, task: TaskKind) -> Result<(ProfileSource, RoutingProfile), RoutingError> {
        match self.settings.read(profile_key(task))? {
            None => Ok((ProfileSource::Default, RoutingProfile::default_for(task))),
            Some(raw) => Ok((ProfileSource::Global, RoutingProfile::from_json(&raw)?)),
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
        // (A valid custom ID is likewise skipped for agent tasks: unknown
        // capability fails closed rather than auto-running tools.)
        let listed = first_listed("openai");
        let entries = vec![
            entry("openai", "not-a-listed-model"),
            entry("openai", &listed),
        ];
        assert_eq!(resolve_route(&entries, true), Some(&entries[1]));
    }

    #[test]
    fn custom_model_resolves_for_chat_and_skips_for_agent() {
        // Valid custom IDs (#67) pass through for text-first tasks on a
        // supported provider — the only resolution path for providers with
        // no hardcoded list, such as the user-configured endpoint.
        let chat_only = vec![entry("openai_compat", "my-custom.model:v1")];
        assert_eq!(resolve_route(&chat_only, false), Some(&chat_only[0]));
        // Unknown capability fails closed: the same entry never resolves
        // for agent tasks.
        assert_eq!(resolve_route(&chat_only, true), None);

        // With a listed fallback behind it, agent resolution skips the
        // custom head and lands on the explicit next entry.
        let listed = first_listed("openai");
        let mixed = vec![
            entry("openai", "my-custom.model:v1"),
            entry("openai", &listed),
        ];
        assert_eq!(resolve_route(&mixed, false), Some(&mixed[0]));
        assert_eq!(resolve_route(&mixed, true), Some(&mixed[1]));
    }

    #[test]
    fn chat_still_skips_charset_invalid_models() {
        // Pass-through admits only *valid* custom IDs: an identifier outside
        // both the provider list and the custom charset is skipped even for
        // chat, rather than passed through.
        let listed = first_listed("anthropic");
        let entries = vec![
            entry("anthropic", "not a listed model"),
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

        // When every entry fails gating, resolution is None even though
        // other providers list plenty of valid models: no implicit fallback.
        // (Charset-invalid models fail even the chat custom pass-through.)
        let hopeless = vec![
            entry("openai", "not a listed model"),
            entry("gemini", "also not listed"),
        ];
        assert_eq!(resolve_route(&hopeless, false), None);
        assert_eq!(resolve_route(&hopeless, true), None);
        assert!(resolve_route(&[], true).is_none());

        // Unknown providers never resolve — not even for a valid custom ID
        // in chat: handing execution a provider with no executor would only
        // fail later, so the order-driven skip applies in both modes.
        let ghost = vec![entry("ghost-provider", "my-custom.model:v1")];
        assert_eq!(resolve_route(&ghost, false), None);
        assert_eq!(resolve_route(&ghost, true), None);
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
    fn default_provider_lists_a_tool_capable_model() {
        // Pins the `default_for` fallback arm as dead code in practice: the
        // registry head always lists a tool-capable model, so the agent
        // default never degrades to a model that resolves to None. If this
        // fails, the defaults need an explicit redesign (see docs).
        let registry = crate::infrastructure::providers::supported_providers();
        let first = registry.first().expect("build must register a provider");
        assert!(
            !crate::application::execution::recommended_models(&first.name, true).is_empty(),
            "default provider {:?} must list a tool-capable model",
            first.name
        );
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

    // ------------------------------------------------------------------
    // Workspace → global → default precedence (`RoutingService::resolve`)
    // ------------------------------------------------------------------

    /// Scratch workspace root unique to this test binary run.
    fn resolve_temp_root() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "nexora-routing-resolve-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp root");
        crate::application::workspace::strip_verbatim(
            dir.canonicalize().expect("canonicalize temp root"),
        )
    }

    fn seed_global(db: &crate::infrastructure::database::Database, task: TaskKind, raw: &str) {
        crate::application::settings::SettingsService::new(db)
            .write(profile_key(task), Some(raw))
            .expect("seed global profile");
    }

    fn seed_workspace(ws: &std::path::Path, task: TaskKind, document: &str) {
        crate::application::project_dir::init_nexora_dir(ws).expect("init succeeds");
        std::fs::write(
            ws.join(".nexora")
                .join("profiles")
                .join(profile_file_name(task)),
            document,
        )
        .expect("seed workspace profile");
    }

    fn workspace_doc(provider: &str, model: &str) -> String {
        format!(r#"[{{"provider":{provider:?},"model":{model:?}}}]"#)
    }

    #[test]
    fn task_labels_parse_to_exactly_two_kinds() {
        assert_eq!(TaskKind::parse("chat"), Some(TaskKind::Chat));
        assert_eq!(TaskKind::parse("agent"), Some(TaskKind::Agent));
        for hostile in [
            "",
            "Chat",
            "CHAT",
            " chat",
            "chat ",
            "ghost",
            "../chat",
            "sk-live-hostile",
            "chat.json",
        ] {
            assert_eq!(
                TaskKind::parse(hostile),
                None,
                "label {hostile:?} must not parse"
            );
        }
        assert_eq!(profile_file_name(TaskKind::Chat), "chat.json");
        assert_eq!(profile_file_name(TaskKind::Agent), "agent.json");
    }

    #[test]
    fn resolve_prefers_valid_workspace_file_over_global() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = RoutingService::new(&db);
        // Global prefers openai; the workspace file prefers gemini.
        let openai_model = first_listed("openai");
        let gemini_model = first_listed("gemini");
        seed_global(&db, TaskKind::Chat, &workspace_doc("openai", &openai_model));
        let ws = resolve_temp_root();
        seed_workspace(&ws, TaskKind::Chat, &workspace_doc("gemini", &gemini_model));
        let resolved = service
            .resolve(Some(ws.as_path()), TaskKind::Chat)
            .expect("resolve succeeds");
        assert_eq!(resolved.source, ProfileSource::Workspace);
        assert_eq!(resolved.notice, None);
        assert_eq!(
            resolved.profile.entries,
            vec![entry("gemini", &gemini_model)]
        );
        // The routed entry follows the workspace order through the existing
        // gating primitive.
        assert_eq!(
            resolved.route(false).map(|found| found.model.as_str()),
            Some(gemini_model.as_str())
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn resolve_invalid_workspace_falls_back_to_global_with_notice() {
        const SENTINEL: &str = "sk-test-sentinel-88ee";
        let db = crate::infrastructure::database::in_memory_database();
        let service = RoutingService::new(&db);
        let listed = first_listed("openai");
        seed_global(&db, TaskKind::Agent, &workspace_doc("openai", &listed));
        let ws = resolve_temp_root();
        seed_workspace(
            &ws,
            TaskKind::Agent,
            &format!(r#"[{{"provider":"ghost-provider","model":{SENTINEL:?}}}]"#),
        );
        let resolved = service
            .resolve(Some(ws.as_path()), TaskKind::Agent)
            .expect("invalid workspace never fails the run");
        assert_eq!(resolved.source, ProfileSource::Global);
        assert_eq!(resolved.notice, Some(INVALID_WORKSPACE_PROFILE_NOTICE));
        assert_eq!(resolved.profile.entries, vec![entry("openai", &listed)]);
        // The fixed-vocab notice echoes no content, file name, or task label.
        let rendered = format!("{:?}", resolved.notice);
        for hostile in [
            "sk-test-sentinel-88ee",
            "ghost-provider",
            "agent",
            "chat.json",
        ] {
            assert!(
                !rendered.to_lowercase().contains(hostile),
                "fallback notice must stay fixed-vocabulary, found {hostile:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn resolve_absent_workspace_file_reads_global_silently() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = RoutingService::new(&db);
        let listed = first_listed("openai");
        seed_global(&db, TaskKind::Chat, &workspace_doc("openai", &listed));
        // Initialized workspace, but no profile file for the task.
        let ws = resolve_temp_root();
        crate::application::project_dir::init_nexora_dir(&ws).expect("init succeeds");
        let resolved = service
            .resolve(Some(ws.as_path()), TaskKind::Chat)
            .expect("resolve succeeds");
        assert_eq!(resolved.source, ProfileSource::Global);
        assert_eq!(resolved.notice, None);
        assert_eq!(resolved.profile.entries, vec![entry("openai", &listed)]);
        // An uninitialized workspace behaves the same (zero behavior change:
        // the global tier decides, silently).
        let bare = resolve_temp_root();
        let resolved = service
            .resolve(Some(bare.as_path()), TaskKind::Chat)
            .expect("resolve succeeds");
        assert_eq!(resolved.source, ProfileSource::Global);
        assert_eq!(resolved.notice, None);
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&bare);
    }

    #[test]
    fn resolve_without_workspace_or_global_reads_defaults() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = RoutingService::new(&db);
        for task in [TaskKind::Chat, TaskKind::Agent] {
            // No workspace root at all: registry defaults, no notice.
            let resolved = service.resolve(None, task).expect("resolve succeeds");
            assert_eq!(resolved.source, ProfileSource::Default);
            assert_eq!(resolved.notice, None);
            assert_eq!(resolved.profile, RoutingProfile::default_for(task));
        }
        // An initialized-but-empty workspace agrees (absent file → global
        // tier → absent key → defaults).
        let ws = resolve_temp_root();
        crate::application::project_dir::init_nexora_dir(&ws).expect("init succeeds");
        let resolved = service
            .resolve(Some(ws.as_path()), TaskKind::Chat)
            .expect("resolve succeeds");
        assert_eq!(resolved.source, ProfileSource::Default);
        assert_eq!(resolved.notice, None);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn resolve_invalid_workspace_without_global_reads_defaults_with_notice() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = RoutingService::new(&db);
        let ws = resolve_temp_root();
        seed_workspace(&ws, TaskKind::Chat, "not json at all sk-test-sentinel-12ab");
        let resolved = service
            .resolve(Some(ws.as_path()), TaskKind::Chat)
            .expect("invalid workspace never fails the run");
        assert_eq!(resolved.source, ProfileSource::Default);
        assert_eq!(resolved.notice, Some(INVALID_WORKSPACE_PROFILE_NOTICE));
        assert_eq!(
            resolved.profile,
            RoutingProfile::default_for(TaskKind::Chat)
        );
        assert!(!format!("{:?}", resolved.notice).contains("sk-test-sentinel-12ab"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn resolve_corrupt_global_still_errors_like_load() {
        // Zero behavior change on the global tier: a corrupt stored value is
        // reported, never silently replaced — with or without a workspace.
        let db = crate::infrastructure::database::in_memory_database();
        let service = RoutingService::new(&db);
        crate::application::settings::SettingsService::new(&db)
            .write(profile_key(TaskKind::Chat), Some("[]"))
            .expect("seed corrupt global");
        assert!(service.load(TaskKind::Chat).is_err());
        assert!(service.resolve(None, TaskKind::Chat).is_err());
        let ws = resolve_temp_root();
        crate::application::project_dir::init_nexora_dir(&ws).expect("init succeeds");
        assert!(service.resolve(Some(ws.as_path()), TaskKind::Chat).is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn save_then_resolve_integration() {
        // The save path feeds the resolution path: a saved workspace document
        // wins on the next resolve.
        let db = crate::infrastructure::database::in_memory_database();
        let service = RoutingService::new(&db);
        let listed = first_listed("anthropic");
        seed_global(
            &db,
            TaskKind::Chat,
            &workspace_doc("openai", &first_listed("openai")),
        );
        let ws = resolve_temp_root();
        crate::application::project_dir::init_nexora_dir(&ws).expect("init succeeds");
        crate::application::project_dir::save_profile_file(
            &ws,
            profile_file_name(TaskKind::Chat),
            &workspace_doc("anthropic", &listed),
        )
        .expect("save succeeds");
        let resolved = service
            .resolve(Some(ws.as_path()), TaskKind::Chat)
            .expect("resolve succeeds");
        assert_eq!(resolved.source, ProfileSource::Workspace);
        assert_eq!(resolved.notice, None);
        assert_eq!(resolved.profile.entries, vec![entry("anthropic", &listed)]);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn per_task_files_resolve_independently() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = RoutingService::new(&db);
        let ws = resolve_temp_root();
        seed_workspace(
            &ws,
            TaskKind::Chat,
            &workspace_doc("openai", &first_listed("openai")),
        );
        // Agent has no workspace file: global tier decides for agent only.
        let resolved_chat = service
            .resolve(Some(ws.as_path()), TaskKind::Chat)
            .expect("chat resolves");
        assert_eq!(resolved_chat.source, ProfileSource::Workspace);
        let resolved_agent = service
            .resolve(Some(ws.as_path()), TaskKind::Agent)
            .expect("agent resolves");
        assert_eq!(resolved_agent.source, ProfileSource::Default);
        assert_eq!(
            resolved_agent.profile,
            RoutingProfile::default_for(TaskKind::Agent)
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}
