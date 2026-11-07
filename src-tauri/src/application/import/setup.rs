// ---------------------------------------------------------------------------
// Setup import: VS Code settings + MCP servers + setup documents (WS-E.2)
// ---------------------------------------------------------------------------
//
// Reads a VS Code `settings.json` and an MCP servers JSON file, translates
// the mappable subset into Nexora settings/routing keys, and re-imports the
// portable setup documents written by
// [`SetupExportService`](crate::application::export::SetupExportService) —
// all through the hardened import/export paths (the [`MAX_IMPORT_JSON_BYTES`]
// cap, the [`contains_secret`] denial, secret-free [`ImportError`]s, and the
// settings store; no new tables).
//
// # Mapping table (hardcoded, documented)
//
// Every writable Nexora settings key (the settings-command gate in
// `commands/settings.rs`) was inspected for a genuine VS Code counterpart.
// Exactly one survives; everything else is reported as skipped, never
// guessed:
//
// | VS Code key | Nexora key | Translation |
// |---|---|---|
// | `workbench.colorTheme` | `appearance.theme` | Unambiguous dark/light
// mention → `dark` / `light` ([`translate_vscode_theme`]); anything else is
// denied as an unsupported value. |
// | `editor.*`, `files.*`, all other keys | — | Skipped: no Nexora
// counterpart exists (no editor font/tab settings, no autosave concept, no
// VS Code provider/model/autonomy/workspace/flag equivalents). |
//
// Considered but rejected: `editor.fontSize` / `editor.tabSize` (Nexora has no
// editor settings at all — inventing keys would be speculative), and any
// theme-name guessing beyond the unambiguous dark/light mention (a theme like
// `Monokai` names no implemented Nexora theme, so mapping it would be a lie).
//
// # MCP storage map
//
// Validated `mcpServers` entries are stored as one JSON array under
// [`MCP_SERVERS_KEY`] in the existing generic settings store (FR-012) —
// `command` + `args` + environment names and values verbatim, except
// secret-like environment material (see below), which denies that server
// entry. The stored shape is validated by [`McpServerList::from_json`], the
// single source shared with the settings-command gate, so both paths agree
// exactly. Later imports replace the stored list (same semantics as
// `set_setting`, never a merge).
//
// # Report secrecy rule
//
// [`SetupImportReport`] echoes caller-supplied *key names* only — the
// checkpoint-label rule from the run snapshots
// (`application/agent/snapshots.rs`): caller-chosen labels may appear in
// read-only views but never in errors. *Values* are never echoed anywhere:
// per-entry denials carry fixed-vocabulary reasons
// ([`DENY_UNSUPPORTED_VALUE`], [`DENY_SECRET_VALUE`], [`DENY_INVALID_RECORD`])
// and every [`ImportError`] display path is fixed vocabulary or a bare
// structural label, exactly like the conversation import errors.
//
// # Hostile input handling
//
// Beyond the shared gates, caller-supplied keys longer than
// [`MAX_SOURCE_KEY_LEN`] deny the whole document with fixed vocabulary (the
// report must not parrot arbitrarily large caller content), and deeply nested
// documents trip `serde_json`'s recursion limit into [`ImportError::InvalidJson`].

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::application::agent::injection::contains_secret;
use crate::application::export::{SETUP_FORMAT, SETUP_VERSION};
use crate::application::flags::{is_known_flag_setting, parse_global_bool};
use crate::application::routing::{
    is_valid_custom_model_id, RoutingProfile, AGENT_PROFILE_KEY, CHAT_PROFILE_KEY,
};
use crate::application::settings::SettingsService;
use crate::application::workspace::{
    parse_recent, parse_registry, WORKSPACE_RECENT_KEY, WORKSPACE_RECENT_MAX, WORKSPACE_ROOTS_KEY,
    WORKSPACE_ROOT_KEY, WORKSPACE_ROOT_MAX_LEN,
};
use crate::infrastructure::database::Database;
use crate::infrastructure::providers::supported_providers;

use super::{classify_error, ensure_valid, ImportError, Result, MAX_IMPORT_JSON_BYTES};

/// Settings key holding the validated MCP server list (a JSON array of
/// [`McpServerEntry`]) in the existing generic settings store (FR-012).
/// Accepted by the settings-command gate exactly when
/// [`McpServerList::from_json`] accepts the value, so both paths agree.
pub(crate) use nexora_core::config::MCP_SERVERS_KEY;

// The setting key strings are owned by the shared, Tauri-free core
// (`nexora-core`, issue #160) and re-exported here, so the setup import and
// the settings write gate cannot drift apart by accident — there is one
// definition of every key name. The `gate_agrees_with_setup_import_allowlist`
// test in `commands/settings.rs` still pins both paths to identical verdicts.
use nexora_core::config::{
    APPEARANCE_THEME_KEY, AUTONOMY_KEY, SELECTED_MODEL_KEY, SELECTED_PROVIDER_KEY,
};

/// Value bound mirrored from the `app_settings` CHECK (DATABASE.md §7.6):
/// `length(value) <= 10000`. Any storable settings value — including the
/// serialized [`McpServerList`] — must fit inside it.
pub(crate) const SETTINGS_VALUE_MAX_LEN: usize = 10_000;

/// Cap (bytes) on a caller-supplied source key (VS Code dotted key, MCP
/// server name, setup key) echoed in a [`SetupImportReport`]. Longer keys
/// deny the whole document with fixed vocabulary instead of being echoed.
const MAX_SOURCE_KEY_LEN: usize = 512;

/// Maximum MCP servers accepted in one import document or one stored list.
pub(crate) const MAX_MCP_SERVERS: usize = 32;

/// Maximum characters in one MCP server name.
const MAX_MCP_NAME_LEN: usize = 128;

/// Maximum characters in one MCP server command.
const MAX_MCP_COMMAND_LEN: usize = 1024;

/// Maximum arguments accepted on one MCP server entry.
const MAX_MCP_ARGS: usize = 64;

/// Maximum characters in one MCP server argument.
const MAX_MCP_ARG_LEN: usize = 1024;

/// Maximum environment variables accepted on one MCP server entry.
const MAX_MCP_ENV_VARS: usize = 64;

/// Maximum characters in one MCP environment variable name.
const MAX_MCP_ENV_NAME_LEN: usize = 128;

/// Maximum characters in one MCP environment variable value.
const MAX_MCP_ENV_VALUE_LEN: usize = 4096;

/// VS Code `settings.json` keys with a genuine Nexora counterpart:
/// `(source key, Nexora settings key)`. See the mapping table in the
/// section docs above: today exactly one entry survives inspection.
const VSCODE_SETTING_MAP: &[(&str, &str)] = &[("workbench.colorTheme", APPEARANCE_THEME_KEY)];

/// Fixed-vocabulary denial reason: the source key maps to Nexora, but its
/// value is outside the Nexora domain (never the offending value).
pub(crate) const DENY_UNSUPPORTED_VALUE: &str = "unsupported value";

/// Fixed-vocabulary denial reason: the entry carries secret-like material
/// (never the offending value).
pub(crate) const DENY_SECRET_VALUE: &str = "secret-like value denied";

/// Fixed-vocabulary denial reason: the entry is structurally invalid
/// (never the offending content).
pub(crate) const DENY_INVALID_RECORD: &str = "invalid record";

/// Translate a VS Code color-theme name to the implemented Nexora appearance
/// theme: an unambiguous whole-word dark/light mention (case-insensitive,
/// non-alphanumeric boundaries) maps to `dark` / `light`; anything else (a
/// theme naming neither, naming both, or matching only as a substring such
/// as `flight` / `delightful`) yields [`None`] so the caller denies the key
/// instead of guessing.
/// Fail-closed on ambiguous/non-word matches.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let mut start = 0;
    while let Some(offset) = haystack[start..].find(needle) {
        let index = start + offset;
        let before_ok = haystack[..index]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let after_ok = haystack[index + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        start = index + needle.len();
    }
    false
}

fn translate_vscode_theme(value: &str) -> Option<&'static str> {
    let lowered = value.to_lowercase();
    match (
        contains_word(&lowered, "dark"),
        contains_word(&lowered, "light"),
    ) {
        (true, false) => Some("dark"),
        (false, true) => Some("light"),
        _ => None,
    }
}

/// One successfully translated key: `(source key, Nexora key)`. Key names
/// are caller-file content echoed in a read-only view (checkpoint-label
/// rule); values are never carried here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ImportedEntry {
    /// The caller-supplied source key (VS Code dotted key,
    /// `mcpServers.<name>`, or setup key).
    pub source_key: String,
    /// The Nexora settings key written.
    pub nexora_key: String,
}

/// One rejected key: the source key plus a fixed-vocabulary reason
/// ([`DENY_UNSUPPORTED_VALUE`], [`DENY_SECRET_VALUE`],
/// [`DENY_INVALID_RECORD`]). Values are never carried here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DeniedEntry {
    /// The caller-supplied source key.
    pub source_key: String,
    /// Fixed, secret-free reason category.
    pub reason: &'static str,
}

/// The per-key outcome of a setup import (WS-E.2): translated keys land in
/// `imported`, keys with no Nexora counterpart land in `skipped`, and mapped
/// keys with unusable values land in `denied`. Secret-free by construction:
/// key names only, fixed-vocabulary reasons, never values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SetupImportReport {
    /// Successfully translated and stored keys.
    pub imported: Vec<ImportedEntry>,
    /// Source keys with no Nexora counterpart (never guessed).
    pub skipped: Vec<String>,
    /// Mapped keys whose values could not be stored, with fixed reasons.
    pub denied: Vec<DeniedEntry>,
}

/// One validated MCP server: a command to launch plus its arguments and
/// environment. Identifiers and plain configuration only — secret-like
/// environment material is denied at validation, never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct McpServerEntry {
    /// Server name (the `mcpServers` object key).
    pub name: String,
    /// Executable command for the server.
    pub command: String,
    /// Command arguments, verbatim.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables, names to values (non-secret only).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl McpServerEntry {
    /// Validate one server record, returning a fixed-vocabulary denial
    /// reason on the first problem. Structural bounds keep the record
    /// storable; the [`contains_secret`] scan over every field keeps
    /// secret-like material out of the settings store (the predicate returns
    /// only `bool`, so a detected secret can never echo).
    fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.name.is_empty()
            || self.name.len() > MAX_MCP_NAME_LEN
            || self.name.contains("..")
            || self.name.chars().any(char::is_control)
        {
            return Err(DENY_INVALID_RECORD);
        }
        if contains_secret(&self.name) || contains_secret(&self.command) {
            return Err(DENY_SECRET_VALUE);
        }
        if self.command.is_empty() || self.command.len() > MAX_MCP_COMMAND_LEN {
            return Err(DENY_INVALID_RECORD);
        }
        if self.args.len() > MAX_MCP_ARGS {
            return Err(DENY_INVALID_RECORD);
        }
        for arg in &self.args {
            if arg.len() > MAX_MCP_ARG_LEN {
                return Err(DENY_INVALID_RECORD);
            }
            if contains_secret(arg) {
                return Err(DENY_SECRET_VALUE);
            }
        }
        if self.env.len() > MAX_MCP_ENV_VARS {
            return Err(DENY_INVALID_RECORD);
        }
        for (name, value) in &self.env {
            if name.is_empty()
                || name.len() > MAX_MCP_ENV_NAME_LEN
                || name.chars().any(char::is_control)
            {
                return Err(DENY_INVALID_RECORD);
            }
            if value.len() > MAX_MCP_ENV_VALUE_LEN {
                return Err(DENY_INVALID_RECORD);
            }
            if contains_secret(name) || contains_secret(value) {
                return Err(DENY_SECRET_VALUE);
            }
        }
        Ok(())
    }
}

/// The validated MCP server list stored under [`MCP_SERVERS_KEY`]: a JSON
/// array of [`McpServerEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct McpServerList {
    /// Validated servers in import order.
    pub servers: Vec<McpServerEntry>,
}

impl McpServerList {
    /// Parse a stored MCP server list, rejecting malformed or out-of-domain
    /// payloads with secret-free [`ImportError`]s.
    ///
    /// Single source shared by the settings-command gate and the setup
    /// import, so both paths classify stored values identically (structural
    /// bounds, duplicate names, the [`contains_secret`] scan, and the
    /// [`SETTINGS_VALUE_MAX_LEN`] fit are all enforced here).
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidJson`] when the input is not valid JSON,
    /// or [`ImportError::InvalidData`] with fixed vocabulary when the payload
    /// violates the structural bounds or carries secret-like material.
    pub(crate) fn from_json(raw: &str) -> Result<Self> {
        ensure_valid(
            raw.len() <= SETTINGS_VALUE_MAX_LEN,
            "stored MCP server list exceeds the settings value limit",
        )?;
        let servers: Vec<McpServerEntry> = serde_json::from_str(raw).map_err(classify_error)?;
        let list = Self { servers };
        list.validate()?;
        Ok(list)
    }

    /// Serialize a validated list for the settings store, enforcing the
    /// [`SETTINGS_VALUE_MAX_LEN`] fit so the value always satisfies the
    /// `app_settings` CHECK (DATABASE.md §7.6).
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidData`] with fixed vocabulary when the
    /// list violates the structural bounds or the serialized form does not
    /// fit the settings value limit.
    pub(crate) fn to_json(&self) -> Result<String> {
        self.validate()?;
        let raw = serde_json::to_string(&self.servers).map_err(classify_error)?;
        ensure_valid(
            raw.len() <= SETTINGS_VALUE_MAX_LEN,
            "MCP server list exceeds the settings value limit",
        )?;
        Ok(raw)
    }

    /// Check structural bounds: at most [`MAX_MCP_SERVERS`] entries, unique
    /// names, and every entry passing [`McpServerEntry::validate`].
    fn validate(&self) -> Result<()> {
        ensure_valid(
            self.servers.len() <= MAX_MCP_SERVERS,
            "MCP server list carries too many entries",
        )?;
        let mut names = HashSet::new();
        for server in &self.servers {
            ensure_valid(
                names.insert(server.name.as_str()),
                "MCP server list repeats a server name",
            )?;
            if let Err(reason) = server.validate() {
                return Err(ImportError::InvalidData(reason.to_string()));
            }
        }
        Ok(())
    }
}

/// A setup document decoded from JSON: the portable format written by
/// [`SetupExportService`](crate::application::export::SetupExportService).
/// `settings` maps each key to its value ([`None`] for `NULL`, which clears
/// the key back to its default on import).
#[derive(Debug, Deserialize)]
struct SetupDocument {
    /// Document kind marker; must equal
    /// [`SETUP_FORMAT`](crate::application::export::SETUP_FORMAT).
    format: String,
    /// Document layout version; must equal
    /// [`SETUP_VERSION`](crate::application::export::SETUP_VERSION).
    version: i64,
    /// The exported settings rows.
    settings: BTreeMap<String, Option<String>>,
}

/// Whether `key` may be written by a setup import at all: the eight
/// gate-owned settings keys, the workspace-roots registry key, the two
/// routing profile keys, the registered `flags.*` keys, and
/// [`MCP_SERVERS_KEY`]. Anything else is skipped, never stored.
fn is_allowlisted_key(key: &str) -> bool {
    matches!(
        key,
        APPEARANCE_THEME_KEY
            | SELECTED_PROVIDER_KEY
            | SELECTED_MODEL_KEY
            | AUTONOMY_KEY
            | WORKSPACE_ROOT_KEY
            | WORKSPACE_RECENT_KEY
            | WORKSPACE_ROOTS_KEY
            | CHAT_PROFILE_KEY
            | AGENT_PROFILE_KEY
            | MCP_SERVERS_KEY
    ) || is_known_flag_setting(key)
}

/// Strict `agent.workspace_recent` domain check, mirroring the
/// settings-command gate exactly: the value must be a JSON array of at most
/// [`WORKSPACE_RECENT_MAX`] non-empty strings within
/// [`WORKSPACE_ROOT_MAX_LEN`] that round-trips through [`parse_recent`].
fn is_valid_recent_value(value: &str) -> bool {
    let items = parse_recent(Some(value));
    match serde_json::from_str::<Vec<String>>(value) {
        Ok(list) => {
            list.len() <= WORKSPACE_RECENT_MAX
                && list
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.len() <= WORKSPACE_ROOT_MAX_LEN)
                && items.len() == list.len()
        }
        Err(_) => false,
    }
}

/// Strict `agent.workspace_roots` domain check, mirroring the
/// settings-command gate exactly: the value must be a JSON array (any length
/// — the registry never evicts) of non-empty strings within
/// [`WORKSPACE_ROOT_MAX_LEN`] that round-trips through [`parse_registry`].
fn is_valid_registry_value(value: &str) -> bool {
    let items = parse_registry(Some(value));
    match serde_json::from_str::<Vec<String>>(value) {
        Ok(list) => {
            list.iter()
                .all(|s| !s.trim().is_empty() && s.len() <= WORKSPACE_ROOT_MAX_LEN)
                && items.len() == list.len()
        }
        Err(_) => false,
    }
}

/// Classify an allowlisted `(key, value)` pair: [`None`] when the value may
/// be stored, or the fixed-vocabulary denial reason otherwise. Domain logic
/// mirrors the settings-command gate key for key (theme/provider/model/
/// autonomy values, workspace syntax, [`RoutingProfile::from_json`], the
/// [`parse_global_bool`] flag domain, [`McpServerList::from_json`]);
/// [`is_importable_setting`] is the boolean projection both the setup import
/// and the gate-agreement test use.
fn setup_denial_reason(key: &str, value: &str) -> Option<&'static str> {
    if value.len() > SETTINGS_VALUE_MAX_LEN {
        return Some(DENY_INVALID_RECORD);
    }
    match key {
        APPEARANCE_THEME_KEY => {
            if matches!(value, "dark" | "light") {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        SELECTED_PROVIDER_KEY => {
            if supported_providers().iter().any(|p| p.name == value) {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        SELECTED_MODEL_KEY => {
            if supported_providers()
                .iter()
                .any(|p| p.models.iter().any(|m| m == value))
                || is_valid_custom_model_id(value)
            {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        AUTONOMY_KEY => {
            if matches!(value, "supervised" | "semi_autonomous" | "full_autonomous") {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        WORKSPACE_ROOT_KEY => {
            if !value.trim().is_empty()
                && !value.contains('\0')
                && value.len() <= WORKSPACE_ROOT_MAX_LEN
            {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        WORKSPACE_RECENT_KEY => {
            if is_valid_recent_value(value) {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        WORKSPACE_ROOTS_KEY => {
            if is_valid_registry_value(value) {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        CHAT_PROFILE_KEY | AGENT_PROFILE_KEY => {
            if RoutingProfile::from_json(value).is_ok() {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        MCP_SERVERS_KEY => {
            if McpServerList::from_json(value).is_ok() {
                None
            } else {
                Some(DENY_INVALID_RECORD)
            }
        }
        _ if is_known_flag_setting(key) => {
            if parse_global_bool(value).is_some() {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        _ => None,
    }
}

/// Whether `(key, value)` may be stored by a setup import: allowlisted key
/// ([`is_allowlisted_key`]) with an in-domain value
/// ([`setup_denial_reason`] reporting no reason).
///
/// Single boolean projection shared by the setup import and the
/// `gate_agrees_with_setup_import_allowlist` test in `commands/settings.rs`,
/// which pins it to the settings-command gate verdict for every sampled
/// input.
///
/// # Panics
///
/// Never panics: all checks are bounded string predicates.
#[must_use]
pub(crate) fn is_importable_setting(key: &str, value: &str) -> bool {
    is_allowlisted_key(key) && setup_denial_reason(key, value).is_none()
}

/// Deny an oversized caller-supplied source key with fixed vocabulary before
/// it can be echoed into a [`SetupImportReport`].
fn check_source_key(key: &str) -> Result<()> {
    ensure_valid(
        key.len() <= MAX_SOURCE_KEY_LEN,
        "import document carries an oversized key",
    )
}

/// Decode one `mcpServers` entry into a validated [`McpServerEntry`],
/// returning the fixed-vocabulary denial reason for the report when the
/// entry is unusable. Unknown fields are ignored (forward compatibility);
/// `args` and `env` default to empty when absent.
fn parse_mcp_entry(
    name: &str,
    def: &serde_json::Value,
) -> std::result::Result<McpServerEntry, &'static str> {
    let object = def.as_object().ok_or(DENY_INVALID_RECORD)?;
    let command = object
        .get("command")
        .and_then(serde_json::Value::as_str)
        .ok_or(DENY_INVALID_RECORD)?;
    let mut args = Vec::new();
    if let Some(raw_args) = object.get("args") {
        let list = raw_args.as_array().ok_or(DENY_INVALID_RECORD)?;
        for arg in list {
            args.push(arg.as_str().ok_or(DENY_INVALID_RECORD)?.to_string());
        }
    }
    let mut env = BTreeMap::new();
    if let Some(raw_env) = object.get("env") {
        let map = raw_env.as_object().ok_or(DENY_INVALID_RECORD)?;
        for (key, val) in map {
            env.insert(
                key.clone(),
                val.as_str().ok_or(DENY_INVALID_RECORD)?.to_string(),
            );
        }
    }
    let entry = McpServerEntry {
        name: name.to_string(),
        command: command.to_string(),
        args,
        env,
    };
    entry.validate()?;
    Ok(entry)
}

/// Application-layer service that imports VS Code settings, MCP servers,
/// and Nexora setup documents into the existing settings store (WS-E.2).
///
/// Every document is fully decoded and validated before any write, so an
/// invalid document performs no writes at all. Settings keys are
/// independent rows with no cross-key invariants, so multi-key imports apply
/// sequentially once validated (unlike the conversation import, no single
/// transaction is needed for coherence). All persistence is delegated to
/// [`SettingsService`]: no SQL, no new tables.
pub(crate) struct SetupImportService<'a> {
    settings: SettingsService<'a>,
}

impl<'a> SetupImportService<'a> {
    /// Create a setup import service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            settings: SettingsService::new(db),
        }
    }

    /// Import a VS Code `settings.json` document (WS-E.2).
    ///
    /// The size cap and the whole-document [`contains_secret`] scan fire
    /// before any parse or write (VS Code settings never legitimately carry
    /// key-like material). Mapped keys are translated per
    /// [`VSCODE_SETTING_MAP`] and stored; unmapped keys are skipped; mapped
    /// keys with unusable values are denied with fixed vocabulary.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidJson`] when the input is not valid JSON,
    /// [`ImportError::InvalidData`] when the document is oversized, carries
    /// key-like material or an oversized key, or is not a JSON object, or
    /// [`ImportError::Database`] when a settings write fails.
    pub(crate) fn import_vscode(&self, json: &str) -> Result<SetupImportReport> {
        ensure_valid(
            json.len() <= MAX_IMPORT_JSON_BYTES,
            "import document exceeds the 16 MiB size limit",
        )?;
        ensure_valid(
            !contains_secret(json),
            "import document contains key-like material",
        )?;
        let value: serde_json::Value = serde_json::from_str(json).map_err(classify_error)?;
        let object = value.as_object().ok_or_else(|| {
            ImportError::InvalidData("VS Code settings document is not a JSON object".to_string())
        })?;
        let mut report = SetupImportReport::default();
        for (source_key, raw) in object {
            check_source_key(source_key)?;
            let Some(nexora_key) = VSCODE_SETTING_MAP
                .iter()
                .find(|(from, _)| *from == source_key)
                .map(|(_, to)| *to)
            else {
                report.skipped.push(source_key.clone());
                continue;
            };
            // Fail closed on future table rows: only wired targets translate.
            let translated = match nexora_key {
                APPEARANCE_THEME_KEY => raw.as_str().and_then(translate_vscode_theme),
                _ => None,
            };
            let Some(translated) = translated else {
                report.denied.push(DeniedEntry {
                    source_key: source_key.clone(),
                    reason: DENY_UNSUPPORTED_VALUE,
                });
                continue;
            };
            self.settings.write(nexora_key, Some(translated))?;
            report.imported.push(ImportedEntry {
                source_key: source_key.clone(),
                nexora_key: nexora_key.to_string(),
            });
        }
        Ok(report)
    }

    /// Import an MCP servers document (`{ "mcpServers": { name: {
    /// `command`, `args`, `env` } } }`, WS-E.2).
    ///
    /// The size cap fires before any parse or write. Unlike
    /// [`Self::import_vscode`], no whole-document [`contains_secret`] scan
    /// applies: `env` blocks legitimately carry key names such as `API_KEY`,
    /// so a whole-document scan would deny the primary use case. Each entry
    /// is scanned with the same predicate instead
    /// ([`McpServerEntry::validate`]): secret-like entries are denied per
    /// server with a secret-free reason while valid servers still import. The
    /// accepted list replaces any previously stored list when non-empty; an
    /// empty accepted list stores nothing and preserves the existing list.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidJson`] when the input is not valid JSON,
    /// [`ImportError::InvalidData`] when the document is oversized, carries
    /// an oversized key, is not an `mcpServers` object, names too many
    /// servers, or the accepted list does not fit the settings value limit,
    /// or [`ImportError::Database`] when the settings write fails.
    pub(crate) fn import_mcp(&self, json: &str) -> Result<SetupImportReport> {
        ensure_valid(
            json.len() <= MAX_IMPORT_JSON_BYTES,
            "import document exceeds the 16 MiB size limit",
        )?;
        let value: serde_json::Value = serde_json::from_str(json).map_err(classify_error)?;
        let servers = value
            .get("mcpServers")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                ImportError::InvalidData("MCP document is not an mcpServers object".to_string())
            })?;
        ensure_valid(
            servers.len() <= MAX_MCP_SERVERS,
            "MCP document carries too many server entries",
        )?;
        let mut accepted = Vec::new();
        let mut report = SetupImportReport::default();
        for (name, def) in servers {
            check_source_key(name)?;
            let source_key = format!("mcpServers.{name}");
            match parse_mcp_entry(name, def) {
                Ok(entry) => accepted.push(entry),
                Err(reason) => report.denied.push(DeniedEntry { source_key, reason }),
            }
        }
        let list = McpServerList { servers: accepted };
        let raw = list.to_json()?;
        if !list.servers.is_empty() {
            self.settings.write(MCP_SERVERS_KEY, Some(&raw))?;
        }
        for server in &list.servers {
            report.imported.push(ImportedEntry {
                source_key: format!("mcpServers.{}", server.name),
                nexora_key: MCP_SERVERS_KEY.to_string(),
            });
        }
        Ok(report)
    }

    /// Import a portable Nexora setup document written by
    /// [`SetupExportService`](crate::application::export::SetupExportService)
    /// (WS-E.2).
    ///
    /// The size cap and the whole-document [`contains_secret`] scan fire
    /// before any parse or write (setup documents never legitimately carry
    /// key-like material: credentials live in the OS keyring, never in
    /// settings). Allowlisted keys with in-domain values are written ([`None`]
    /// values delete the key, restoring its default); unknown keys are
    /// skipped; allowlisted keys with out-of-domain values are denied with
    /// fixed vocabulary.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidJson`] when the input is not valid JSON,
    /// [`ImportError::UnsupportedFormat`] or [`ImportError::UnsupportedVersion`]
    /// for a non-setup document, [`ImportError::InvalidData`] when the
    /// document is oversized, carries key-like material or an oversized key,
    /// or violates the setup shape, or [`ImportError::Database`] when a
    /// settings write fails.
    pub(crate) fn import_setup(&self, json: &str) -> Result<SetupImportReport> {
        ensure_valid(
            json.len() <= MAX_IMPORT_JSON_BYTES,
            "import document exceeds the 16 MiB size limit",
        )?;
        ensure_valid(
            !contains_secret(json),
            "import document contains key-like material",
        )?;
        let document: SetupDocument = serde_json::from_str(json).map_err(classify_error)?;
        if document.format != SETUP_FORMAT {
            return Err(ImportError::UnsupportedFormat {
                format: document.format.clone(),
            });
        }
        if document.version != SETUP_VERSION {
            return Err(ImportError::UnsupportedVersion {
                version: document.version,
            });
        }
        let mut report = SetupImportReport::default();
        for (key, value) in &document.settings {
            check_source_key(key)?;
            if !is_allowlisted_key(key) {
                report.skipped.push(key.clone());
                continue;
            }
            let Some(stored) = value.as_deref() else {
                self.settings.delete(key)?;
                report.imported.push(ImportedEntry {
                    source_key: key.clone(),
                    nexora_key: key.clone(),
                });
                continue;
            };
            if let Some(reason) = setup_denial_reason(key, stored) {
                report.denied.push(DeniedEntry {
                    source_key: key.clone(),
                    reason,
                });
                continue;
            }
            self.settings.write(key, Some(stored))?;
            report.imported.push(ImportedEntry {
                source_key: key.clone(),
                nexora_key: key.clone(),
            });
        }
        Ok(report)
    }
}

#[cfg(test)]
mod setup_import_tests {
    use super::*;
    use crate::application::export::SetupExportService;
    use crate::infrastructure::database::in_memory_database;

    const SECRET_SENTINELS: [&str; 3] = ["sk-live-sentinel-42", "sk-", "api_key"];

    fn setup_db() -> Database {
        in_memory_database()
    }

    /// [`SetupExportService::serialize_setup`] with a freshly minted
    /// `export_setup` id (each export consumes a single-use id, so every
    /// confirmed call mints its own).
    fn serialize_setup_confirmed(
        service: &SetupExportService<'_>,
    ) -> crate::application::export::Result<String> {
        let registry = crate::application::confirmations::ConfirmationRegistry::new();
        let id = registry
            .request(
                crate::application::confirmations::SCOPE_EXPORT,
                &crate::application::confirmations::ConfirmationTarget::new(
                    crate::application::confirmations::OP_EXPORT_SETUP,
                ),
            )
            .expect("mint export confirmation");
        service.serialize_setup(&id, &registry)
    }

    /// Assert `rendered` echoes none of the hostile sentinels (case-insensitive).
    fn assert_secret_free(rendered: &str) {
        for sentinel in SECRET_SENTINELS {
            assert!(
                !rendered.to_lowercase().contains(sentinel),
                "output must stay secret-free, found {sentinel:?} in {rendered:?}"
            );
        }
    }

    #[test]
    fn vscode_theme_mapping_hits_dark_and_light() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "workbench.colorTheme": "Default Dark Modern",
        })
        .to_string();

        let report = service.import_vscode(&json).expect("theme import succeeds");

        assert_eq!(
            report.imported,
            vec![ImportedEntry {
                source_key: "workbench.colorTheme".to_string(),
                nexora_key: "appearance.theme".to_string(),
            }]
        );
        assert!(report.skipped.is_empty());
        assert!(report.denied.is_empty());
        assert_eq!(
            SettingsService::new(&db)
                .read("appearance.theme")
                .expect("read theme"),
            Some("dark".to_string())
        );

        // A light theme overwrites through the same mapped key.
        let json = serde_json::json!({ "workbench.colorTheme": "Default Light+" }).to_string();
        let report = service.import_vscode(&json).expect("light import succeeds");
        assert_eq!(report.imported.len(), 1);
        assert_eq!(
            SettingsService::new(&db)
                .read("appearance.theme")
                .expect("read theme"),
            Some("light".to_string())
        );
    }

    #[test]
    fn vscode_unmapped_keys_are_skipped_and_store_nothing() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "editor.fontSize": 14,
            "editor.tabSize": 4,
            "files.autoSave": "afterDelay",
            "workbench.startupEditor": "newUntitledFile",
        })
        .to_string();

        let report = service
            .import_vscode(&json)
            .expect("unmapped import succeeds");

        assert!(report.imported.is_empty());
        assert!(report.denied.is_empty());
        let mut skipped = report.skipped.clone();
        skipped.sort();
        assert_eq!(
            skipped,
            vec![
                "editor.fontSize".to_string(),
                "editor.tabSize".to_string(),
                "files.autoSave".to_string(),
                "workbench.startupEditor".to_string(),
            ]
        );
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "skipped keys store nothing"
        );
    }

    #[test]
    fn vscode_unsupported_theme_value_is_denied_secret_free() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        // Names neither implemented theme: mappable key, unusable value.
        let json = serde_json::json!({ "workbench.colorTheme": "Monokai Dimmed" }).to_string();

        let report = service
            .import_vscode(&json)
            .expect("denial lands in the report");

        assert!(report.imported.is_empty());
        assert!(report.skipped.is_empty());
        assert_eq!(
            report.denied,
            vec![DeniedEntry {
                source_key: "workbench.colorTheme".to_string(),
                reason: DENY_UNSUPPORTED_VALUE,
            }]
        );
        // The report echoes the key name (caller content, checkpoint-label
        // rule) but never the value.
        let rendered = format!("{report:?}");
        assert!(rendered.contains("workbench.colorTheme"));
        assert!(!rendered.contains("Monokai Dimmed"));
        assert!(
            SettingsService::new(&db)
                .read("appearance.theme")
                .expect("read theme")
                .is_none(),
            "denied values store nothing"
        );
    }

    #[test]
    fn vscode_theme_substring_lookalikes_are_denied() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        // Substring-only matches (`flight` / `delightful`), ambiguous
        // mentions (`Dark Light`), and the empty name must all fail closed.
        for name in ["Dark Light", "", "Night Flight", "Delightful"] {
            let json = serde_json::json!({ "workbench.colorTheme": name }).to_string();
            let report = service
                .import_vscode(&json)
                .expect("denial lands in the report");
            assert!(report.imported.is_empty(), "name {name:?} must not import");
            assert!(report.skipped.is_empty(), "name {name:?} must not skip");
            assert_eq!(
                report.denied,
                vec![DeniedEntry {
                    source_key: "workbench.colorTheme".to_string(),
                    reason: DENY_UNSUPPORTED_VALUE,
                }],
                "name {name:?} must be denied"
            );
        }
        assert!(
            SettingsService::new(&db)
                .read("appearance.theme")
                .expect("read theme")
                .is_none(),
            "denied lookalikes store nothing"
        );
    }

    #[test]
    fn vscode_secret_bearing_document_is_denied_without_echo() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "workbench.colorTheme": "Default Dark",
            "http.proxyPassword": "sk-live-sentinel-42",
        })
        .to_string();

        let err = service
            .import_vscode(&json)
            .expect_err("secret must be denied");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("key-like")),
            "secret denial must use fixed vocabulary, got {err:?}"
        );
        assert_secret_free(&format!("{err}"));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "denied documents write nothing"
        );
    }

    #[test]
    fn vscode_oversize_document_is_denied_before_any_write() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let padding = "x".repeat(MAX_IMPORT_JSON_BYTES);
        let json = serde_json::json!({ "workbench.colorTheme": padding }).to_string();
        assert!(json.len() > MAX_IMPORT_JSON_BYTES);

        let err = service
            .import_vscode(&json)
            .expect_err("oversize must be denied");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("16 MiB")),
            "oversize denial must name the pinned cap, got {err:?}"
        );
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "oversize documents write nothing"
        );
    }

    #[test]
    fn vscode_hostile_shapes_are_handled_by_the_gates() {
        let db = setup_db();
        let service = SetupImportService::new(&db);

        // A non-object document is invalid data.
        let err = service.import_vscode("[]").expect_err("array must fail");
        assert!(matches!(err, ImportError::InvalidData(_)));

        // Deep nesting trips the JSON recursion limit into invalid JSON.
        let mut nested = "null".to_string();
        for _ in 0..300 {
            nested = format!("{{\"k\":{nested}}}");
        }
        let err = service
            .import_vscode(&nested)
            .expect_err("deep nesting must fail");
        assert!(matches!(err, ImportError::InvalidJson(_)));

        // A huge key denies the whole document with fixed vocabulary rather
        // than being echoed into the report.
        let huge_key = "k".repeat(MAX_SOURCE_KEY_LEN + 1);
        let json = format!("{{\"{huge_key}\": 1}}");
        let err = service
            .import_vscode(&json)
            .expect_err("huge key must fail");
        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("oversized key")),
            "huge keys deny the document, got {err:?}"
        );
        assert!(!format!("{err}").contains(&huge_key));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "hostile documents write nothing"
        );
    }

    #[test]
    fn mcp_import_stores_validated_servers() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "mcpServers": {
                "github": {
                    "command": "npx",
                    "args": ["-y", "github-mcp"],
                    "env": { "PORT": "8080" },
                },
                "notes": { "command": "/usr/local/bin/notes-mcp" },
            },
        })
        .to_string();

        let report = service.import_mcp(&json).expect("mcp import succeeds");

        assert_eq!(report.skipped.len(), 0);
        assert_eq!(report.denied.len(), 0);
        assert_eq!(report.imported.len(), 2);
        for entry in &report.imported {
            assert_eq!(entry.nexora_key, MCP_SERVERS_KEY);
            assert!(entry.source_key.starts_with("mcpServers."));
        }
        let raw = SettingsService::new(&db)
            .read(MCP_SERVERS_KEY)
            .expect("read servers")
            .expect("servers stored");
        let list = McpServerList::from_json(&raw).expect("stored list validates");
        assert_eq!(list.servers.len(), 2);
        let github = list
            .servers
            .iter()
            .find(|server| server.name == "github")
            .expect("github stored");
        assert_eq!(github.command, "npx");
        assert_eq!(github.args, vec!["-y", "github-mcp"]);
        assert_eq!(github.env.get("PORT").map(String::as_str), Some("8080"));
    }

    #[test]
    fn mcp_secret_env_denies_only_that_server_without_echo() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "mcpServers": {
                "clean": { "command": "clean-mcp" },
                "leaky": {
                    "command": "leaky-mcp",
                    "env": { "GITHUB_TOKEN": "sk-live-sentinel-42" },
                },
            },
        })
        .to_string();

        let report = service
            .import_mcp(&json)
            .expect("partial mcp import succeeds");

        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.imported[0].source_key, "mcpServers.clean");
        assert_eq!(
            report.denied,
            vec![DeniedEntry {
                source_key: "mcpServers.leaky".to_string(),
                reason: DENY_SECRET_VALUE,
            }]
        );
        assert_secret_free(&format!("{report:?}"));
        let raw = SettingsService::new(&db)
            .read(MCP_SERVERS_KEY)
            .expect("read servers")
            .expect("clean server stored");
        assert_secret_free(&raw);
        let list = McpServerList::from_json(&raw).expect("stored list validates");
        assert_eq!(list.servers.len(), 1);
        assert_eq!(list.servers[0].name, "clean");
    }

    #[test]
    fn mcp_traversal_name_is_denied_and_stores_nothing_for_it() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "mcpServers": {
                "../evil": { "command": "evil-mcp" },
                "fine": { "command": "fine-mcp" },
            },
        })
        .to_string();

        let report = service
            .import_mcp(&json)
            .expect("partial mcp import succeeds");

        assert_eq!(report.imported.len(), 1);
        assert_eq!(
            report.denied,
            vec![DeniedEntry {
                source_key: "mcpServers.../evil".to_string(),
                reason: DENY_INVALID_RECORD,
            }]
        );
        let raw = SettingsService::new(&db)
            .read(MCP_SERVERS_KEY)
            .expect("read servers")
            .expect("fine server stored");
        let list = McpServerList::from_json(&raw).expect("stored list validates");
        assert_eq!(list.servers.len(), 1);
        assert_eq!(list.servers[0].name, "fine");
    }

    #[test]
    fn mcp_oversize_document_is_denied_before_any_write() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let padding = "x".repeat(MAX_IMPORT_JSON_BYTES);
        let json = serde_json::json!({
            "mcpServers": { "big": { "command": padding } },
        })
        .to_string();
        assert!(json.len() > MAX_IMPORT_JSON_BYTES);

        let err = service
            .import_mcp(&json)
            .expect_err("oversize must be denied");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("16 MiB")),
            "oversize denial must name the pinned cap, got {err:?}"
        );
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "oversize documents write nothing"
        );
    }

    #[test]
    fn mcp_aggregate_overflow_denies_the_document() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        // Enough medium servers to pass every per-server bound while the
        // serialized list no longer fits the settings value limit.
        let mut servers = serde_json::Map::new();
        for index in 0..MAX_MCP_SERVERS {
            servers.insert(
                format!("srv-{index:02}"),
                serde_json::json!({ "command": "c".repeat(300) }),
            );
        }
        let json = serde_json::json!({ "mcpServers": servers }).to_string();
        assert!(json.len() <= MAX_IMPORT_JSON_BYTES);

        let err = service
            .import_mcp(&json)
            .expect_err("aggregate overflow must fail");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("value limit")),
            "aggregate overflow must name the value limit, got {err:?}"
        );
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "overflowed documents write nothing"
        );
    }

    #[test]
    fn setup_round_trip_export_import_is_stable() {
        let db = setup_db();
        let settings = SettingsService::new(&db);
        settings
            .write("appearance.theme", Some("dark"))
            .expect("seed theme");
        settings
            .write("flags.assembly", Some("false"))
            .expect("seed flag");
        let mcp = McpServerList {
            servers: vec![McpServerEntry {
                name: "github".to_string(),
                command: "npx".to_string(),
                args: vec!["-y".to_string()],
                env: BTreeMap::from([("PORT".to_string(), "8080".to_string())]),
            }],
        };
        settings
            .write(MCP_SERVERS_KEY, Some(&mcp.to_json().expect("encode mcp")))
            .expect("seed mcp");
        let first =
            serialize_setup_confirmed(&SetupExportService::new(&db)).expect("export succeeds");

        // Import into a fresh database, then export again: stable.
        let fresh = setup_db();
        let report = SetupImportService::new(&fresh)
            .import_setup(&first)
            .expect("setup re-import succeeds");
        assert!(report.skipped.is_empty());
        assert!(report.denied.is_empty());
        assert!(!report.imported.is_empty());
        let second = serialize_setup_confirmed(&SetupExportService::new(&fresh))
            .expect("re-export succeeds");
        assert_eq!(first, second, "export → import must round-trip cleanly");
    }

    #[test]
    fn setup_import_skips_unknown_keys_and_deletes_nulls() {
        let db = setup_db();
        let settings = SettingsService::new(&db);
        settings
            .write("appearance.theme", Some("dark"))
            .expect("seed theme");
        let json = serde_json::json!({
            "format": "nexora-setup",
            "version": 1,
            "settings": {
                "appearance.theme": null,
                "flags.assembly": "false",
                "editor.fontSize": "14",
            },
        })
        .to_string();

        let report = SetupImportService::new(&db)
            .import_setup(&json)
            .expect("setup import succeeds");

        // The null clears the seeded theme back to its default (absent).
        assert!(
            settings
                .read("appearance.theme")
                .expect("read theme")
                .is_none(),
            "null setup values delete the key"
        );
        assert_eq!(
            settings.read("flags.assembly").expect("read flag"),
            Some("false".to_string())
        );
        assert_eq!(report.skipped, vec!["editor.fontSize".to_string()]);
        assert!(report.denied.is_empty());
        assert_eq!(report.imported.len(), 2);
    }

    #[test]
    fn setup_import_rejects_wrong_format_and_version() {
        let db = setup_db();
        let service = SetupImportService::new(&db);

        let json = serde_json::json!({
            "format": "some-other-format",
            "version": 1,
            "settings": {},
        })
        .to_string();
        let err = service
            .import_setup(&json)
            .expect_err("wrong format must fail");
        assert!(matches!(err, ImportError::UnsupportedFormat { .. }));

        let json = serde_json::json!({
            "format": "nexora-setup",
            "version": 2,
            "settings": {},
        })
        .to_string();
        let err = service
            .import_setup(&json)
            .expect_err("wrong version must fail");
        assert!(matches!(
            err,
            ImportError::UnsupportedVersion { version: 2 }
        ));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "rejected documents write nothing"
        );
    }

    #[test]
    fn setup_import_denies_out_of_domain_values_secret_free() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "format": "nexora-setup",
            "version": 1,
            "settings": {
                "appearance.theme": "ultraviolet",
                "flags.assembly": "maybe",
            },
        })
        .to_string();

        let report = service
            .import_setup(&json)
            .expect("denials land in the report");

        assert!(report.imported.is_empty());
        assert!(report.skipped.is_empty());
        assert_eq!(report.denied.len(), 2);
        for denial in &report.denied {
            assert_eq!(denial.reason, DENY_UNSUPPORTED_VALUE);
        }
        let rendered = format!("{report:?}");
        assert!(!rendered.contains("ultraviolet"));
        assert!(!rendered.contains("maybe"));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "denied values store nothing"
        );
    }

    #[test]
    fn setup_secret_bearing_document_is_denied_without_echo() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "format": "nexora-setup",
            "version": 1,
            "settings": { "appearance.theme": "sk-live-sentinel-42" },
        })
        .to_string();

        let err = service
            .import_setup(&json)
            .expect_err("secret must be denied");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("key-like")),
            "secret denial must use fixed vocabulary, got {err:?}"
        );
        assert_secret_free(&format!("{err}"));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "denied documents write nothing"
        );
    }
}
