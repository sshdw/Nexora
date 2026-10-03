//! Feature flags for the 2.0 rollout (workspace → global → default).
//!
//! The four 2.0 subsystems that are always-on in `main` each get one gate:
//!
//! | Flag | Subsystem | New path gated | Flag OFF reproduces |
//! |---|---|---|
//! | `snapshots` | WS-C.1 | [`SnapshotStore`](crate::application::agent::snapshots::SnapshotStore)
//! capture / checkpoint / rollback | no snapshots (pre-2.0: no store) |
//! | `self_audit` | WS-C.3 | [`record_report`](crate::application::agent::self_audit::record_report)
//! outcome append | no outcome append (pass stays read-only) |
//! | `injection` | WS-C.2 | untrusted-output envelopes plus the marker-scan
//! approval hold | raw observations, no scan (pre-2.0 dispatch) |
//! | `assembly` | smart assembly | budgeted opening build plus the proactive
//! compaction seed | legacy unbounded build (pre-2.0 prompts) |
//!
//! Precedence mirrors the routing profiles
//! ([`RoutingService::resolve`](crate::application::routing::RoutingService::resolve)):
//! a valid workspace `.nexora/flags.json` file wins; otherwise the
//! app-global `flags.*` settings keys decide; otherwise the hardcoded
//! [`FlagDef::default`] applies. Every default is the current `main` behavior
//! (`true`), so resolving with nothing set changes nothing. A present
//! workspace file that fails to load falls back to the global tier with the
//! fixed-vocabulary [`INVALID_WORKSPACE_FLAGS_NOTICE`] (never failing the
//! run, never echoing content) — the same fallback rule as the profile
//! precedence.
//!
//! # Enforcement status (phased rollout)
//!
//! `injection` and `assembly` are enforced in the run path today: the run
//! bridge resolves [`ResolvedFlags::run_flags`] once per run and the runner
//! consumes it
//! ([`AgentRunner::with_run_flags`](crate::application::agent::runner::AgentRunner::with_run_flags)).
//! `snapshots` and `self_audit` are resolved-but-not-yet-enforced: their
//! values and sources resolve through the same precedence and appear in the
//! status view, but no snapshot or self-audit record path reads them yet, so
//! setting `{"snapshots": false}` changes the reported value without
//! changing run behavior. The status view marks this per flag
//! ([`FlagStatus::enforced`], pinned by `status_marks_only_enforced_flags`)
//! until snapshot and self-audit recording land in the run path.
//!
//! The workspace document is a flat JSON object of known-name boolean values
//! (`{"snapshots": false}`); anything else (malformed JSON, a non-object, an
//! unknown flag name, a non-boolean value) is invalid and triggers the
//! fallback. [`FlagSet::from_json`] is the parse-then-validate single source:
//! the file loader ([`crate::application::project_dir`]) only guards and reads
//! text, while global-value parsing ([`parse_global_bool`]) is shared with the
//! settings-command gate so both paths classify values identically.
//!
//! # Security
//!
//! Flags carry booleans only. [`FlagError`] messages use fixed category text
//! so even adversarial file content can never be echoed into an error.

use std::collections::{BTreeMap, HashMap};

use crate::application::settings::SettingsService;
use crate::infrastructure::database::Database;

/// Settings-key prefix holding the app-global flag values (`flags.<name>`).
pub(crate) const FLAG_SETTING_PREFIX: &str = "flags.";

/// Fixed-vocabulary notice returned alongside the flags when a present
/// workspace flags file fails to load and resolution falls back to the global
/// settings keys (or the hardcoded defaults when no global key is set).
///
/// Mirrors
/// [`INVALID_WORKSPACE_PROFILE_NOTICE`](crate::application::routing::INVALID_WORKSPACE_PROFILE_NOTICE):
/// fixed vocabulary that echoes no document content, no file name, and no flag
/// name. The run never fails for a bad workspace file; the fallback applies
/// and the note travels on [`ResolvedFlags::notice`].
pub(crate) const INVALID_WORKSPACE_FLAGS_NOTICE: &str =
    "the workspace flags file is invalid; using the stored settings flags";

/// One registered feature flag: its name, its default (the current `main`
/// behavior), and a short description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FlagDef {
    /// Flag name: the workspace-document key and the `flags.*` settings suffix.
    pub name: &'static str,
    /// Hardcoded default: always the current `main` behavior (enabled), so
    /// resolving with nothing set changes nothing.
    pub default: bool,
    /// Short human-readable description of the gated subsystem.
    pub description: &'static str,
}

/// The hardcoded flag table: the exact set of 2.0 gates that exist — no
/// speculative flags. Every default is `true` (current behavior).
pub(crate) const FLAGS: &[FlagDef] = &[
    FlagDef {
        name: "snapshots",
        default: true,
        description: "run snapshot capture, checkpoints, and rollback (WS-C.1)",
    },
    FlagDef {
        name: "self_audit",
        default: true,
        description: "self-audit outcome recording on the audit trail (WS-C.3)",
    },
    FlagDef {
        name: "injection",
        default: true,
        description: "untrusted-output envelopes and the marker-scan approval hold (WS-C.2)",
    },
    FlagDef {
        name: "assembly",
        default: true,
        description: "budgeted smart context assembly of the run opening",
    },
];

/// Whether `name` is a registered flag.
#[must_use]
pub(crate) fn is_known_flag(name: &str) -> bool {
    FLAGS.iter().any(|flag| flag.name == name)
}

/// Whether `key` is a writable global flag setting (`flags.<known-name>`).
#[must_use]
pub(crate) fn is_known_flag_setting(key: &str) -> bool {
    key.strip_prefix(FLAG_SETTING_PREFIX)
        .is_some_and(is_known_flag)
}

/// Parse an app-global flag value: trimmed, case-insensitive `true` / `false`
/// only. Anything else yields [`None`] (treated as absent → hardcoded
/// default), mirroring the autonomy/preset resolvers.
///
/// Single source shared by the service resolution below and the
/// settings-command gate, so both paths classify values identically.
#[must_use]
pub(crate) fn parse_global_bool(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Where one resolved flag value came from (precedence order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlagSource {
    /// A valid workspace `.nexora/flags.json` file.
    Workspace,
    /// The app-global settings key (`flags.<name>`).
    Global,
    /// The hardcoded registry default ([`FlagDef::default`]).
    Default,
}

impl FlagSource {
    /// Fixed vocabulary for the status view (`workspace` / `global` / `default`).
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Global => "global",
            Self::Default => "default",
        }
    }
}

/// One resolved flag: its effective value plus where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedFlag {
    /// Effective value after the workspace → global → default precedence.
    pub enabled: bool,
    /// Where the value came from.
    pub source: FlagSource,
}

/// All four flags resolved for one workspace root, plus the workspace-file
/// fallback notice ([`None`] unless a present file failed to load).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedFlags {
    /// Effective `snapshots` value.
    pub snapshots: bool,
    /// Effective `self_audit` value.
    pub self_audit: bool,
    /// Effective `injection` value.
    pub injection: bool,
    /// Effective `assembly` value.
    pub assembly: bool,
    /// Set to [`INVALID_WORKSPACE_FLAGS_NOTICE`] when a present workspace
    /// file failed to load and the global tier applied; [`None`] otherwise.
    pub notice: Option<&'static str>,
}

impl ResolvedFlags {
    /// Project the run-path subset consumed by the agent runner.
    #[must_use]
    pub(crate) const fn run_flags(self) -> RunFlags {
        RunFlags {
            snapshots: self.snapshots,
            self_audit: self.self_audit,
            injection: self.injection,
            assembly: self.assembly,
        }
    }
}

/// Run-path flag projection consumed by
/// [`AgentRunner::with_run_flags`](crate::application::agent::runner::AgentRunner::with_run_flags).
/// Defaults to the current `main` behavior (everything enabled), so runs
/// constructed without flags behave exactly as before. The four named gates
/// stay explicit fields (rather than a map) so each gate keeps its own
/// documentation at the use site.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunFlags {
    /// Gate the WS-C.1 snapshot record paths.
    pub snapshots: bool,
    /// Gate the WS-C.3 self-audit record path.
    pub self_audit: bool,
    /// Gate the WS-C.2 envelopes plus the marker-scan approval hold.
    pub injection: bool,
    /// Gate the budgeted smart-assembly opening (plus its proactive seed).
    pub assembly: bool,
}

impl Default for RunFlags {
    fn default() -> Self {
        Self {
            snapshots: true,
            self_audit: true,
            injection: true,
            assembly: true,
        }
    }
}

/// One flag's status for the read-only `flags_status` view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct FlagStatus {
    /// Effective value after the workspace → global → default precedence.
    pub enabled: bool,
    /// Fixed source vocabulary (`workspace` / `global` / `default`).
    pub source: &'static str,
    /// Whether the run path enforces this flag today (phased rollout:
    /// `injection`/`assembly` only — see the module docs). Resolved the same
    /// way for every flag; enforcement is a property of the flag name.
    pub enforced: bool,
}

/// Whether `name` is enforced in the run path today (phased rollout:
/// `injection` and `assembly` ride `RunFlags` into the runner; `snapshots`
/// and `self_audit` resolve but gate nothing yet — see the module docs).
/// Unknown names report not enforced (fail-closed, like [`FlagService::is_enabled`]).
#[must_use]
pub(crate) fn is_flag_enforced(name: &str) -> bool {
    matches!(name, "injection" | "assembly")
}

/// Read-only status response for the `flags_status` command: every
/// registered flag mapped to its effective value, fixed-vocabulary source,
/// and enforcement mark, plus the workspace-file fallback notice ([`None`]
/// unless a present file failed to load).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct FlagsStatusView {
    /// Per-flag status keyed by flag name.
    pub flags: BTreeMap<String, FlagStatus>,
    /// [`INVALID_WORKSPACE_FLAGS_NOTICE`] when a present workspace file
    /// failed to load and the global tier applied; [`None`] otherwise.
    pub notice: Option<&'static str>,
}

/// A validated workspace flags document: known-name boolean values only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct FlagSet {
    /// Explicit workspace overrides (`name` → value).
    values: HashMap<String, bool>,
}

impl FlagSet {
    /// Parse a workspace flags document, rejecting malformed or out-of-domain
    /// payloads with fixed, secret-free reasons.
    ///
    /// The document must be a JSON object whose keys are all registered flag
    /// names and whose values are all booleans. An empty object is valid (no
    /// overrides). Unknown keys and non-boolean values fail closed without
    /// echoing the offending content.
    ///
    /// # Errors
    ///
    /// Returns [`FlagError::InvalidDocument`] when the payload is not a JSON
    /// object of known-name boolean values.
    pub(crate) fn from_json(raw: &str) -> Result<Self, FlagError> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| FlagError::InvalidDocument {
                reason: "feature flags document is not valid JSON",
            })?;
        let object = value.as_object().ok_or(FlagError::InvalidDocument {
            reason: "feature flags document is not a JSON object",
        })?;
        let mut values = HashMap::with_capacity(object.len());
        for (name, flag_value) in object {
            if !is_known_flag(name) {
                return Err(FlagError::InvalidDocument {
                    reason: "feature flags document names an unknown flag",
                });
            }
            let Some(enabled) = flag_value.as_bool() else {
                return Err(FlagError::InvalidDocument {
                    reason: "feature flags document carries a non-boolean value",
                });
            };
            values.insert(name.clone(), enabled);
        }
        Ok(Self { values })
    }

    /// Workspace override for `name`, if the document sets one.
    #[must_use]
    fn get(&self, name: &str) -> Option<bool> {
        self.values.get(name).copied()
    }
}

/// Application-layer service resolving feature flags through the
/// workspace → global → default precedence.
pub(crate) struct FlagService<'a> {
    settings: SettingsService<'a>,
}

impl<'a> FlagService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            settings: SettingsService::new(db),
        }
    }

    /// Whether `name` is currently enabled for `workspace_root`.
    ///
    /// Best-effort like the autonomy/preset resolvers: an unreadable settings
    /// tier degrades to the hardcoded default, and an unknown flag name fails
    /// closed to `false`.
    #[must_use]
    pub(crate) fn is_enabled(&self, workspace_root: Option<&std::path::Path>, name: &str) -> bool {
        if !is_known_flag(name) {
            return false;
        }
        self.resolve_all(workspace_root).get(name)
    }

    /// Resolve every registered flag for `workspace_root` in one pass: the
    /// workspace file is loaded at most once, then each flag falls through
    /// workspace → global → default independently.
    ///
    /// Best-effort: an unreadable settings tier degrades to the hardcoded
    /// default, never failing the caller.
    #[must_use]
    pub(crate) fn resolve_all(&self, workspace_root: Option<&std::path::Path>) -> ResolvedFlags {
        let workspace = Self::workspace_tier(workspace_root);
        let notice =
            matches!(workspace, WorkspaceTier::Invalid).then_some(INVALID_WORKSPACE_FLAGS_NOTICE);
        let mut resolved = ResolvedFlags {
            snapshots: true,
            self_audit: true,
            injection: true,
            assembly: true,
            notice,
        };
        for flag in FLAGS {
            let (enabled, _) = self.resolve_one(flag, &workspace);
            resolved.set(flag.name, enabled);
        }
        resolved
    }

    /// Read-only status view for the `flags_status` command: every registered
    /// flag mapped to its effective value, fixed-vocabulary source, and
    /// enforcement mark, plus the workspace-file fallback notice.
    ///
    /// A present-but-invalid workspace file never errors here: resolution
    /// falls back to the global tier and the fixed-vocabulary notice travels
    /// on the returned view.
    ///
    /// # Errors
    ///
    /// Returns [`FlagError::Database`] on a failed settings read.
    pub(crate) fn status(
        &self,
        workspace_root: Option<&std::path::Path>,
    ) -> Result<FlagsStatusView, FlagError> {
        let workspace = Self::workspace_tier(workspace_root);
        let notice =
            matches!(workspace, WorkspaceTier::Invalid).then_some(INVALID_WORKSPACE_FLAGS_NOTICE);
        let mut flags = BTreeMap::new();
        for flag in FLAGS {
            let (enabled, source) = self.resolve_one_strict(flag, &workspace)?;
            flags.insert(
                flag.name.to_string(),
                FlagStatus {
                    enabled,
                    source: source.as_str(),
                    enforced: is_flag_enforced(flag.name),
                },
            );
        }
        Ok(FlagsStatusView { flags, notice })
    }

    /// Resolve one flag against a pre-loaded workspace tier, degrading an
    /// unreadable global tier to the hardcoded default.
    fn resolve_one(&self, flag: &FlagDef, workspace: &WorkspaceTier) -> (bool, FlagSource) {
        if let WorkspaceTier::Present(set) = workspace {
            if let Some(enabled) = set.get(flag.name) {
                return (enabled, FlagSource::Workspace);
            }
        }
        match self.global_value(flag.name) {
            Ok(Some(enabled)) => (enabled, FlagSource::Global),
            _ => (flag.default, FlagSource::Default),
        }
    }

    /// Resolve one flag against a pre-loaded workspace tier, propagating a
    /// failed settings read to the caller.
    fn resolve_one_strict(
        &self,
        flag: &FlagDef,
        workspace: &WorkspaceTier,
    ) -> Result<(bool, FlagSource), FlagError> {
        if let WorkspaceTier::Present(set) = workspace {
            if let Some(enabled) = set.get(flag.name) {
                return Ok((enabled, FlagSource::Workspace));
            }
        }
        match self
            .settings
            .read(&format!("{}{}", FLAG_SETTING_PREFIX, flag.name))?
        {
            Some(raw) => match parse_global_bool(&raw) {
                Some(enabled) => Ok((enabled, FlagSource::Global)),
                None => Ok((flag.default, FlagSource::Default)),
            },
            None => Ok((flag.default, FlagSource::Default)),
        }
    }

    /// Read one app-global flag value: [`Some`] for a parseable stored value,
    /// [`None`] when absent, `NULL`, unparseable, or unreadable (all degrade
    /// to the hardcoded default).
    fn global_value(
        &self,
        name: &str,
    ) -> Result<Option<bool>, crate::infrastructure::database::DatabaseError> {
        match self.settings.read(&format!("{FLAG_SETTING_PREFIX}{name}")) {
            Ok(Some(raw)) => Ok(parse_global_bool(&raw)),
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Load the workspace tier once: absent (no root, no file) reads the
    /// global tier silently; a present file that parses wins; a present file
    /// that fails to load falls back with the fixed-vocab notice.
    fn workspace_tier(workspace_root: Option<&std::path::Path>) -> WorkspaceTier {
        let Some(root) = workspace_root else {
            return WorkspaceTier::Absent;
        };
        if !crate::application::project_dir::flags_file_present(root) {
            return WorkspaceTier::Absent;
        }
        match crate::application::project_dir::load_flags_text(root)
            .ok()
            .and_then(|text| FlagSet::from_json(&text).ok())
        {
            Some(set) => WorkspaceTier::Present(set),
            None => WorkspaceTier::Invalid,
        }
    }
}

impl ResolvedFlags {
    /// Set one flag value by registry name (the table is fixed, so every name
    /// matches exactly one field; anything else is a programming error).
    fn set(&mut self, name: &str, enabled: bool) {
        match name {
            "snapshots" => self.snapshots = enabled,
            "self_audit" => self.self_audit = enabled,
            "injection" => self.injection = enabled,
            "assembly" => self.assembly = enabled,
            _ => {
                debug_assert!(
                    matches!(name, "snapshots" | "self_audit" | "injection" | "assembly"),
                    "ResolvedFlags::set reached with an unregistered flag"
                );
            }
        }
    }

    /// Read one flag value by registry name.
    fn get(&self, name: &str) -> bool {
        match name {
            "snapshots" => self.snapshots,
            "self_audit" => self.self_audit,
            "injection" => self.injection,
            "assembly" => self.assembly,
            _ => {
                debug_assert!(
                    matches!(name, "snapshots" | "self_audit" | "injection" | "assembly"),
                    "ResolvedFlags::get reached with an unregistered flag"
                );
                false
            }
        }
    }
}

/// Workspace tier of the flag precedence: loaded at most once per resolution.
#[derive(Debug)]
enum WorkspaceTier {
    /// No workspace root, or no flags file: the global tier decides silently.
    Absent,
    /// A valid workspace document: its overrides win per flag.
    Present(FlagSet),
    /// A present file that failed to load: the global tier applies with the
    /// fixed-vocabulary notice.
    Invalid,
}

/// Errors raised by flags parsing and status reads.
///
/// Messages are fixed category text: they never echo the offending payload,
/// so formatting a [`FlagError`] cannot leak a value that was mistakenly
/// stored under a flags key.
#[derive(Debug)]
pub(crate) enum FlagError {
    /// A workspace flags document failed structural validation.
    InvalidDocument {
        /// Fixed, secret-free reason category.
        reason: &'static str,
    },
    /// A settings-store read failed.
    Database(crate::infrastructure::database::DatabaseError),
}

impl std::fmt::Display for FlagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDocument { reason } => write!(f, "invalid feature flags: {reason}"),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for FlagError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidDocument { .. } => None,
            Self::Database(err) => Some(err),
        }
    }
}

impl From<crate::infrastructure::database::DatabaseError> for FlagError {
    fn from(err: crate::infrastructure::database::DatabaseError) -> Self {
        Self::Database(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        crate::infrastructure::database::in_memory_database()
    }

    fn seed_global(db: &Database, name: &str, value: &str) {
        SettingsService::new(db)
            .write(&format!("{FLAG_SETTING_PREFIX}{name}"), Some(value))
            .expect("seed global flag");
    }

    fn temp_root() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("nexora-flags-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp root");
        crate::application::workspace::strip_verbatim(
            dir.canonicalize().expect("canonicalize temp root"),
        )
    }

    fn seed_workspace(ws: &std::path::Path, document: &str) {
        crate::application::project_dir::init_nexora_dir(ws).expect("init succeeds");
        std::fs::write(ws.join(".nexora").join("flags.json"), document).expect("seed flags file");
    }

    #[test]
    fn registry_lists_exactly_the_four_gates_all_defaulting_on() {
        let names: Vec<&str> = FLAGS.iter().map(|flag| flag.name).collect();
        assert_eq!(
            names,
            vec!["snapshots", "self_audit", "injection", "assembly"]
        );
        for flag in FLAGS {
            assert!(
                flag.default,
                "flag {:?} must default to current behavior",
                flag.name
            );
            assert!(!flag.description.is_empty());
        }
        assert!(is_known_flag("snapshots"));
        assert!(!is_known_flag("ghost-flag"));
        assert!(!is_known_flag(""));
        assert!(is_known_flag_setting("flags.assembly"));
        assert!(!is_known_flag_setting("flags.ghost-flag"));
        assert!(!is_known_flag_setting("flags."));
        assert!(!is_known_flag_setting("routing.profile.chat"));
    }

    #[test]
    fn global_values_parse_strictly() {
        assert_eq!(parse_global_bool("true"), Some(true));
        assert_eq!(parse_global_bool("TRUE"), Some(true));
        assert_eq!(parse_global_bool("  True  "), Some(true));
        assert_eq!(parse_global_bool("false"), Some(false));
        assert_eq!(parse_global_bool("FALSE"), Some(false));
        for hostile in [
            "",
            "yes",
            "no",
            "1",
            "0",
            "on",
            "off",
            "2",
            "truthy",
            "sk-live-secret",
        ] {
            assert_eq!(
                parse_global_bool(hostile),
                None,
                "value {hostile:?} must not parse"
            );
        }
    }

    #[test]
    fn workspace_document_parses_partial_objects() {
        let set = FlagSet::from_json(r#"{"snapshots": false}"#).expect("partial parses");
        assert_eq!(set.get("snapshots"), Some(false));
        assert_eq!(set.get("assembly"), None);
        let empty = FlagSet::from_json("{}").expect("empty parses");
        assert_eq!(empty.get("snapshots"), None);
        let full = FlagSet::from_json(
            r#"{"snapshots": true, "self_audit": false, "injection": true, "assembly": false}"#,
        )
        .expect("full parses");
        assert_eq!(
            (
                full.get("snapshots"),
                full.get("self_audit"),
                full.get("injection"),
                full.get("assembly")
            ),
            (Some(true), Some(false), Some(true), Some(false))
        );
    }

    #[test]
    fn workspace_document_rejects_out_of_domain_secret_free() {
        const SENTINELS: [&str; 2] = ["sk-", "top-secret-value"];
        let bad = [
            "not json at all",
            "[",
            "[]",
            "true",
            r#"{"snapshots": "false"}"#,
            r#"{"snapshots": 0}"#,
            r#"{"snapshots": null}"#,
            r#"{"ghost-flag": true}"#,
            r#"{"snapshots": true, "ghost-flag": false}"#,
            r#"{"SNAPSHOTS": true}"#,
            r#"{"sk-top-secret-value": true}"#,
        ];
        for raw in bad {
            let err = FlagSet::from_json(raw).expect_err("bad flags must fail");
            let rendered = format!("{err}");
            assert!(
                rendered.starts_with("invalid feature flags: "),
                "got {rendered:?}"
            );
            for sentinel in SENTINELS {
                assert!(
                    !rendered.to_lowercase().contains(sentinel),
                    "flags error must stay secret-free, found {sentinel:?} in {rendered:?}"
                );
            }
        }
    }

    #[test]
    fn precedence_is_workspace_then_global_then_default() {
        let db = test_db();
        let service = FlagService::new(&db);
        // Nothing set anywhere: hardcoded defaults, no notice.
        let resolved = service.resolve_all(None);
        assert_eq!(
            (
                resolved.snapshots,
                resolved.self_audit,
                resolved.injection,
                resolved.assembly
            ),
            (true, true, true, true)
        );
        assert_eq!(resolved.notice, None);

        // Global tier decides when no workspace file exists.
        seed_global(&db, "snapshots", "false");
        let resolved = service.resolve_all(None);
        assert!(!resolved.snapshots);
        assert!(resolved.assembly);
        assert_eq!(resolved.notice, None);

        // Workspace wins over global.
        let ws = temp_root();
        seed_workspace(&ws, r#"{"snapshots": true, "assembly": false}"#);
        let resolved = service.resolve_all(Some(ws.as_path()));
        assert!(resolved.snapshots, "workspace true beats global false");
        assert!(!resolved.assembly, "workspace false beats unset global");
        assert!(
            resolved.injection,
            "unset workspace falls to unset global, then default"
        );
        assert_eq!(resolved.notice, None);

        // Per-flag fall-through: a partial workspace document leaves other
        // flags on the global tier.
        seed_global(&db, "injection", "false");
        let resolved = service.resolve_all(Some(ws.as_path()));
        assert!(
            !resolved.injection,
            "global decides flags the workspace omits"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn invalid_workspace_file_falls_back_with_fixed_vocab_notice() {
        const SENTINEL: &str = "sk-test-sentinel-44ab";
        let db = test_db();
        let service = FlagService::new(&db);
        seed_global(&db, "snapshots", "false");
        let ws = temp_root();
        seed_workspace(
            &ws,
            &format!(r#"{{"snapshots": true, "ghost-{SENTINEL}": true}}"#),
        );
        let resolved = service.resolve_all(Some(ws.as_path()));
        assert!(
            !resolved.snapshots,
            "invalid file falls back to the global tier"
        );
        assert_eq!(resolved.notice, Some(INVALID_WORKSPACE_FLAGS_NOTICE));
        assert!(!format!("{resolved:?}")
            .to_lowercase()
            .contains("sk-test-sentinel-44ab"));

        // Without a global value the fallback reaches the hardcoded default,
        // still carrying the notice.
        let bare_db = test_db();
        let bare_service = FlagService::new(&bare_db);
        let resolved = bare_service.resolve_all(Some(ws.as_path()));
        assert!(
            resolved.snapshots,
            "fallback without global reaches the default"
        );
        assert_eq!(resolved.notice, Some(INVALID_WORKSPACE_FLAGS_NOTICE));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn absent_workspace_file_reads_global_silently() {
        let db = test_db();
        let service = FlagService::new(&db);
        seed_global(&db, "assembly", "false");
        let ws = temp_root();
        crate::application::project_dir::init_nexora_dir(&ws).expect("init succeeds");
        let resolved = service.resolve_all(Some(ws.as_path()));
        assert!(!resolved.assembly);
        assert_eq!(resolved.notice, None);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn unparseable_global_value_degrades_to_default() {
        let db = test_db();
        seed_global(&db, "assembly", "sk-test-sentinel-91cc");
        let service = FlagService::new(&db);
        let resolved = service.resolve_all(None);
        assert!(resolved.assembly, "unparseable global reads as the default");
        assert_eq!(resolved.notice, None);
        // The status view agrees and stays secret-free.
        let status = service.status(None).expect("status succeeds");
        assert_eq!(status.notice, None);
        assert_eq!(
            status.flags["assembly"],
            FlagStatus {
                enabled: true,
                source: "default",
                enforced: true,
            }
        );
        let rendered = serde_json::to_string(&status).expect("serialize status");
        assert!(!rendered.contains("sk-test-sentinel-91cc"));
    }

    #[test]
    fn is_enabled_fails_closed_for_unknown_names() {
        let db = test_db();
        let service = FlagService::new(&db);
        assert!(service.is_enabled(None, "snapshots"));
        assert!(!service.is_enabled(None, "ghost-flag"));
        assert!(!service.is_enabled(None, ""));
        seed_global(&db, "snapshots", "false");
        assert!(!service.is_enabled(None, "snapshots"));
    }

    #[test]
    fn status_maps_every_flag_to_enabled_and_source() {
        let db = test_db();
        let service = FlagService::new(&db);
        seed_global(&db, "self_audit", "false");
        let ws = temp_root();
        seed_workspace(&ws, r#"{"injection": false}"#);
        let status = service.status(Some(ws.as_path())).expect("status succeeds");
        assert_eq!(status.notice, None);
        let names: Vec<&str> = status.flags.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            vec!["assembly", "injection", "self_audit", "snapshots"]
        );
        assert_eq!(
            status.flags["snapshots"],
            FlagStatus {
                enabled: true,
                source: "default",
                enforced: false,
            }
        );
        assert_eq!(
            status.flags["self_audit"],
            FlagStatus {
                enabled: false,
                source: "global",
                enforced: false,
            }
        );
        assert_eq!(
            status.flags["injection"],
            FlagStatus {
                enabled: false,
                source: "workspace",
                enforced: true,
            }
        );
        assert_eq!(
            status.flags["assembly"],
            FlagStatus {
                enabled: true,
                source: "default",
                enforced: true,
            }
        );
        // Response-side snake_case shape.
        let rendered = serde_json::to_string(&status).expect("serialize status");
        assert!(rendered.contains("\"enabled\""));
        assert!(rendered.contains("\"source\""));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn status_marks_only_enforced_flags() {
        // Phased rollout (module docs): `injection`/`assembly` are enforced
        // in the run path; `snapshots`/`self_audit` resolve but gate nothing
        // yet. The per-flag mark pins that contract.
        assert!(is_flag_enforced("injection"));
        assert!(is_flag_enforced("assembly"));
        assert!(!is_flag_enforced("snapshots"));
        assert!(!is_flag_enforced("self_audit"));
        assert!(!is_flag_enforced("ghost-flag"));
        let db = test_db();
        let service = FlagService::new(&db);
        let status = service.status(None).expect("status succeeds");
        assert!(status.flags["injection"].enforced);
        assert!(status.flags["assembly"].enforced);
        assert!(!status.flags["snapshots"].enforced);
        assert!(!status.flags["self_audit"].enforced);
        // Additive field on the stable per-flag shape.
        let rendered = serde_json::to_string(&status).expect("serialize status");
        assert!(rendered.contains("\"enforced\""));
    }

    #[test]
    fn status_carries_notice_for_invalid_workspace_file() {
        let db = test_db();
        let service = FlagService::new(&db);
        seed_global(&db, "snapshots", "false");
        // A valid workspace file resolves cleanly with no notice.
        let ws = temp_root();
        seed_workspace(&ws, r#"{"snapshots": true}"#);
        let status = service.status(Some(ws.as_path())).expect("status succeeds");
        assert_eq!(status.notice, None);
        assert!(status.flags["snapshots"].enabled);
        // A present-but-invalid file falls back to the global tier and the
        // fixed-vocabulary notice travels on the view (never an error, never
        // echoing content).
        seed_workspace(&ws, r#"{"ghost-flag": true}"#);
        let status = service.status(Some(ws.as_path())).expect("status succeeds");
        assert!(!status.flags["snapshots"].enabled);
        assert_eq!(status.notice, Some(INVALID_WORKSPACE_FLAGS_NOTICE));
        let rendered = serde_json::to_string(&status).expect("serialize status");
        assert!(rendered.contains(INVALID_WORKSPACE_FLAGS_NOTICE));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn resolved_flags_set_get_round_trip_every_registered_flag() {
        // Every registry entry must survive a set/get round-trip: a missing
        // arm in either accessor (silently absorbing the name) fails here.
        let mut resolved = ResolvedFlags {
            snapshots: true,
            self_audit: true,
            injection: true,
            assembly: true,
            notice: None,
        };
        for flag in FLAGS {
            resolved.set(flag.name, false);
            assert!(
                !resolved.get(flag.name),
                "flag {:?} must round-trip false",
                flag.name
            );
            resolved.set(flag.name, true);
            assert!(
                resolved.get(flag.name),
                "flag {:?} must round-trip true",
                flag.name
            );
        }
    }

    #[test]
    fn run_flags_default_to_current_behavior() {
        let flags = RunFlags::default();
        assert_eq!(
            (
                flags.snapshots,
                flags.self_audit,
                flags.injection,
                flags.assembly
            ),
            (true, true, true, true)
        );
        let resolved = ResolvedFlags {
            snapshots: false,
            self_audit: true,
            injection: false,
            assembly: true,
            notice: None,
        };
        assert_eq!(
            resolved.run_flags(),
            RunFlags {
                snapshots: false,
                self_audit: true,
                injection: false,
                assembly: true,
            }
        );
    }

    #[test]
    fn flag_error_stays_secret_free() {
        const SENTINEL: &str = "sk-live-hostile";
        let err = FlagSet::from_json(&format!(r#"{{"{SENTINEL}": true}}"#))
            .expect_err("unknown hostile key must fail");
        let rendered = format!("{err}");
        assert!(!rendered.to_lowercase().contains("sk-"));
        assert!(!rendered.contains(SENTINEL));
    }
}
