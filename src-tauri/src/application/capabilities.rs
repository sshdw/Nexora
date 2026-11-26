//! `(connection_id, model_id)` Capability Catalog & Provenance (`M06` / P10, P11).
//!
//! Enforces strict precedence:
//! `User Manual Override (app_settings)` > `Provider API Metadata (zero-token /models)`
//! > `Built-in Verified Catalog` > `Unknown`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use nexora_core::config::MODEL_CAPABILITY_OVERRIDES_KEY;
use nexora_core::provider::{ExecutorError, ReasoningEffort};
use serde::{Deserialize, Serialize};

use crate::infrastructure::database::Database;
use crate::infrastructure::providers::catalog::gemini::GEMINI_LITE_MODELS;
use crate::infrastructure::providers::metadata::{
    fetch_connection_metadata, probe_connection_metadata, ApiModelMetadata, MetadataProbeError,
};
use crate::infrastructure::providers::openai::CompatConfig;
use crate::infrastructure::providers::supported_providers;

use super::compat::CompatEndpointService;
use super::context_stats::context_limit_for;
use super::execution::RequestExecutionService;
use super::project_dir;
use super::settings::{
    configured_workspace_root, resolve_effective_setting, save_scoped_setting, ScopedSaveError,
    SettingScope, SettingsService,
};

/// Origin of the highest-precedence capability layer applied for `(connection_id, model_id)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilityProvenance {
    ManualOverride,
    ApiMetadata,
    BuiltInCatalog,
    Unknown,
}

/// Resolved capability matrix for a single `(connection_id, model_id)` pair.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectionModelCapabilities {
    pub connection_id: String,
    pub model_id: String,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub supported_efforts: Vec<String>,
    pub supports_thinking: bool,
    pub supports_attachments: bool,
    pub supports_tools: bool,
    pub provenance: CapabilityProvenance,
    pub has_manual_override: bool,
}

/// Partial or full user manual override persisted in `app_settings`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelCapabilityOverride {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub context_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub supported_efforts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub supports_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub supports_attachments: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub supports_tools: Option<bool>,
}

impl ModelCapabilityOverride {
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.context_window.is_none()
            && self.max_output_tokens.is_none()
            && self.supported_efforts.is_none()
            && self.supports_thinking.is_none()
            && self.supports_attachments.is_none()
            && self.supports_tools.is_none()
    }
}

/// Secret-free diagnostic category returned by `test_provider_connection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionTestCategory {
    Ok,
    AuthFailed,
    Timeout,
    DnsOrProxy,
    TlsError,
    NotRunning,
    HttpError,
}

/// Secret-free diagnostic result returned by `test_provider_connection`.
/// Never echoes API keys, URLs with userinfo, or raw HTTP bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectionTestResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub discovered_count: usize,
    pub category: ConnectionTestCategory,
    pub hint: String,
}

fn override_map_key(connection_id: &str, model_id: &str) -> String {
    format!("{connection_id}::{model_id}")
}

fn normalize_efforts(efforts: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in efforts {
        if let Some(parsed) = ReasoningEffort::parse(raw) {
            let s = parsed.as_str().to_string();
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

fn metadata_cache() -> &'static Mutex<HashMap<(String, String), ApiModelMetadata>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), ApiModelMetadata>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Store fetched zero-token API metadata in the process cache for `(connection_id, model_id)`.
pub(crate) fn cache_api_metadata(connection_id: &str, items: &[ApiModelMetadata]) {
    let mut guard = metadata_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for item in items {
        guard.insert(
            (connection_id.to_string(), item.model_id.clone()),
            item.clone(),
        );
    }
}

#[cfg(test)]
pub(crate) fn clear_api_metadata_cache() {
    metadata_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

/// Read all persisted manual capability overrides from `app_settings`.
pub(crate) fn read_manual_overrides(
    db: &Database,
) -> crate::infrastructure::repository::Result<BTreeMap<String, ModelCapabilityOverride>> {
    let Some(raw) = SettingsService::new(db).read(MODEL_CAPABILITY_OVERRIDES_KEY)? else {
        return Ok(BTreeMap::new());
    };
    Ok(serde_json::from_str::<BTreeMap<String, ModelCapabilityOverride>>(&raw).unwrap_or_default())
}

/// Read effective manual overrides (`Project > Personal`), ignoring corrupt JSON.
pub(crate) fn read_effective_overrides(db: &Database) -> BTreeMap<String, ModelCapabilityOverride> {
    resolve_effective_setting(db, None, MODEL_CAPABILITY_OVERRIDES_KEY, None)
        .and_then(|raw| {
            serde_json::from_str::<BTreeMap<String, ModelCapabilityOverride>>(&raw).ok()
        })
        .unwrap_or_default()
}

/// Persist or clear a manual capability override for `(connection_id, model_id)` in `app_settings`.
pub(crate) fn set_manual_override(
    db: &Database,
    connection_id: &str,
    model_id: &str,
    override_value: Option<ModelCapabilityOverride>,
) -> crate::infrastructure::repository::Result<ConnectionModelCapabilities> {
    let mut map = read_manual_overrides(db)?;
    let key = override_map_key(connection_id, model_id);
    match override_value {
        Some(mut ov) if !ov.is_empty() => {
            if let Some(efforts) = ov.supported_efforts.take() {
                ov.supported_efforts = Some(normalize_efforts(&efforts));
            }
            map.insert(key, ov);
        }
        _ => {
            map.remove(&key);
        }
    }
    if map.is_empty() {
        SettingsService::new(db).delete(MODEL_CAPABILITY_OVERRIDES_KEY)?;
    } else {
        let json = serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string());
        SettingsService::new(db).write(MODEL_CAPABILITY_OVERRIDES_KEY, Some(&json))?;
    }
    Ok(resolve_model_capabilities(db, connection_id, model_id))
}

/// Persist or clear a manual capability override for `(connection_id, model_id)` in either
/// `SettingScope::Personal` (`SQLite`) or `SettingScope::Project` (`.nexora/config.json`).
///
/// # Errors
///
/// Returns [`ScopedSaveError`] when `Project` scope lacks an active workspace
/// or writing `.nexora/config.json` fails validation or security checks.
pub(crate) fn set_scoped_manual_override(
    db: &Database,
    scope: SettingScope,
    connection_id: &str,
    model_id: &str,
    override_value: Option<ModelCapabilityOverride>,
) -> std::result::Result<ConnectionModelCapabilities, ScopedSaveError> {
    let mut map = match scope {
        SettingScope::Personal => read_manual_overrides(db)?,
        SettingScope::Project => {
            let ws = configured_workspace_root(db).ok_or(ScopedSaveError::WorkspaceRequired)?;
            let raw = project_dir::load_project_config_raw(&ws)?;
            raw.entries
                .into_iter()
                .rev()
                .find(|e| e.key == MODEL_CAPABILITY_OVERRIDES_KEY)
                .and_then(|e| {
                    serde_json::from_value::<BTreeMap<String, ModelCapabilityOverride>>(e.value)
                        .ok()
                })
                .unwrap_or_default()
        }
    };
    let key = override_map_key(connection_id, model_id);
    match override_value {
        Some(mut ov) if !ov.is_empty() => {
            if let Some(efforts) = ov.supported_efforts.take() {
                ov.supported_efforts = Some(normalize_efforts(&efforts));
            }
            map.insert(key, ov);
        }
        _ => {
            map.remove(&key);
        }
    }
    let val_opt = if map.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string()))
    };
    let _ = save_scoped_setting(
        db,
        None,
        scope,
        MODEL_CAPABILITY_OVERRIDES_KEY,
        val_opt.as_deref(),
        None,
    )?;
    Ok(resolve_model_capabilities(db, connection_id, model_id))
}

fn from_baseline_spec(
    connection_id: &str,
    model_id: &str,
    spec: &crate::infrastructure::providers::catalog::BaselineModelSpec,
) -> ConnectionModelCapabilities {
    ConnectionModelCapabilities {
        connection_id: connection_id.to_string(),
        model_id: model_id.to_string(),
        context_window: Some(spec.context_window),
        max_output_tokens: Some(spec.max_output_tokens),
        supported_efforts: spec
            .supported_efforts
            .iter()
            .copied()
            .map(ToString::to_string)
            .collect(),
        supports_thinking: spec.supports_thinking,
        supports_attachments: spec.supports_attachments,
        supports_tools: spec.supports_tools,
        provenance: CapabilityProvenance::BuiltInCatalog,
        has_manual_override: false,
    }
}

fn built_in_catalog_entry(
    connection_id: &str,
    model_id: &str,
) -> Option<ConnectionModelCapabilities> {
    if let Some(spec) = crate::infrastructure::providers::catalog::openai::find_builtin_model_spec(
        connection_id,
        model_id,
    ) {
        return Some(from_baseline_spec(connection_id, model_id, spec));
    }

    let is_listed = supported_providers()
        .iter()
        .any(|p| p.name == connection_id && p.models.iter().any(|m| m == model_id));
    if !is_listed {
        return None;
    }

    let limit_u64 = context_limit_for(Some(connection_id), Some(model_id));
    let context_window = u32::try_from(limit_u64).ok();

    let (supported_efforts, supports_thinking, supports_attachments, supports_tools) =
        match connection_id {
            "openai" => (
                vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                    "xhigh".to_string(),
                ],
                true,
                true,
                true,
            ),
            "anthropic" => {
                let efforts = if model_id.contains("haiku") {
                    vec!["low".to_string(), "medium".to_string(), "high".to_string()]
                } else {
                    vec![
                        "low".to_string(),
                        "medium".to_string(),
                        "high".to_string(),
                        "xhigh".to_string(),
                    ]
                };
                (efforts, true, true, true)
            }
            "gemini" => {
                let is_lite = GEMINI_LITE_MODELS.contains(&model_id);
                if is_lite {
                    (Vec::new(), false, true, true)
                } else {
                    (
                        vec!["low".to_string(), "medium".to_string(), "high".to_string()],
                        true,
                        true,
                        true,
                    )
                }
            }
            _ => {
                let thinking_capable = model_id.contains("r1")
                    || model_id.contains("claude")
                    || model_id.contains("gpt-5")
                    || model_id.contains("qwen3")
                    || model_id.contains("gemini")
                    || model_id.contains("reasoning");
                let efforts = if thinking_capable {
                    vec!["low".to_string(), "medium".to_string(), "high".to_string()]
                } else {
                    Vec::new()
                };
                (efforts, thinking_capable, true, true)
            }
        };

    Some(ConnectionModelCapabilities {
        connection_id: connection_id.to_string(),
        model_id: model_id.to_string(),
        context_window,
        max_output_tokens: None,
        supported_efforts,
        supports_thinking,
        supports_attachments,
        supports_tools,
        provenance: CapabilityProvenance::BuiltInCatalog,
        has_manual_override: false,
    })
}

/// Prefix/pattern family rules applied strictly as a lower-precedence fallback
/// behind `ManualOverride` and concrete `ApiMetadata` fields. Never overrides
/// explicit `ApiMetadata` `false` or `ManualOverride`.
fn family_rules_entry(connection_id: &str, model_id: &str) -> Option<ConnectionModelCapabilities> {
    let normalized = model_id.trim().to_ascii_lowercase();
    let bare = normalized
        .rsplit_once('/')
        .map_or(normalized.as_str(), |(_, tail)| tail);
    let bare = bare.split(':').next().unwrap_or(bare);

    let spec = match_family_rule(bare)?;
    Some(ConnectionModelCapabilities {
        connection_id: connection_id.to_string(),
        model_id: model_id.to_string(),
        context_window: Some(spec.context_window),
        max_output_tokens: Some(spec.max_output_tokens),
        supported_efforts: spec
            .supported_efforts
            .iter()
            .copied()
            .map(ToString::to_string)
            .collect(),
        supports_thinking: spec.supports_thinking,
        supports_attachments: spec.supports_attachments,
        supports_tools: spec.supports_tools,
        provenance: CapabilityProvenance::BuiltInCatalog,
        has_manual_override: false,
    })
}

#[derive(Clone, Copy)]
struct FamilyRuleSpec {
    context_window: u32,
    max_output_tokens: u32,
    supported_efforts: &'static [&'static str],
    supports_thinking: bool,
    supports_attachments: bool,
    supports_tools: bool,
}

const LMH: &[&str] = &["low", "medium", "high"];
const LMHX: &[&str] = &["low", "medium", "high", "xhigh"];
const LH: &[&str] = &["low", "high"];

const fn rule(
    context_window: u32,
    max_output_tokens: u32,
    supported_efforts: &'static [&'static str],
    supports_thinking: bool,
    supports_attachments: bool,
    supports_tools: bool,
) -> FamilyRuleSpec {
    FamilyRuleSpec {
        context_window,
        max_output_tokens,
        supported_efforts,
        supports_thinking,
        supports_attachments,
        supports_tools,
    }
}

/// Ordered prefix table (more-specific prefixes precede shorter ones).
const FAMILY_RULES: &[(&str, FamilyRuleSpec)] = &[
    ("gpt-5", rule(400_000, 128_000, LMHX, true, true, true)),
    ("o3", rule(200_000, 100_000, LMH, true, true, true)),
    ("o4", rule(200_000, 100_000, LMH, true, true, true)),
    ("gpt-4.1", rule(1_047_576, 32_768, &[], false, true, true)),
    ("gpt-4o", rule(128_000, 16_384, &[], false, true, true)),
    (
        "claude-opus-4",
        rule(200_000, 32_000, LMHX, true, true, true),
    ),
    (
        "claude-haiku-4",
        rule(200_000, 64_000, LMH, true, true, true),
    ),
    (
        "claude-sonnet-4",
        rule(200_000, 64_000, LMHX, true, true, true),
    ),
    ("claude-4", rule(200_000, 64_000, LMHX, true, true, true)),
    ("claude-3-7", rule(200_000, 64_000, LMH, true, true, true)),
    ("claude-3.7", rule(200_000, 64_000, LMH, true, true, true)),
    ("claude-3-5", rule(200_000, 8_192, &[], false, true, true)),
    ("claude-3.5", rule(200_000, 8_192, &[], false, true, true)),
    (
        "gemini-2.5-flash-lite",
        rule(1_048_576, 65_536, &[], false, true, true),
    ),
    (
        "gemini-flash-lite",
        rule(1_048_576, 65_536, &[], false, true, true),
    ),
    ("gemini-2.5", rule(1_048_576, 65_536, LMH, true, true, true)),
    ("gemini-3", rule(1_048_576, 65_536, LMH, true, true, true)),
    (
        "deepseek-reasoner",
        rule(128_000, 64_000, LMH, true, false, true),
    ),
    ("deepseek-r1", rule(128_000, 64_000, LMH, true, false, true)),
    (
        "deepseek-v4-pro",
        rule(128_000, 64_000, LMH, true, false, true),
    ),
    (
        "deepseek-chat",
        rule(128_000, 8_192, &[], false, false, true),
    ),
    ("deepseek-v3", rule(128_000, 8_192, &[], false, false, true)),
    (
        "deepseek-flash",
        rule(128_000, 8_192, &[], false, false, true),
    ),
    ("grok-4", rule(256_000, 65_536, LMH, true, true, true)),
    ("grok-3-mini", rule(131_072, 32_768, LH, true, false, true)),
    ("grok-3", rule(131_072, 32_768, &[], false, false, true)),
    ("qwen3", rule(131_072, 32_768, LMH, true, false, true)),
    ("qwq", rule(131_072, 32_768, LMH, true, false, true)),
    ("magistral", rule(128_000, 40_960, LMH, true, false, true)),
    ("codestral", rule(256_000, 32_768, &[], false, false, true)),
    ("pixtral", rule(131_072, 16_384, &[], false, true, true)),
    (
        "mistral-large",
        rule(131_072, 16_384, &[], false, false, true),
    ),
];

fn match_family_rule(bare: &str) -> Option<FamilyRuleSpec> {
    FAMILY_RULES
        .iter()
        .find(|(prefix, _)| bare.starts_with(prefix))
        .map(|(_, spec)| *spec)
}

fn unknown_capabilities(connection_id: &str, model_id: &str) -> ConnectionModelCapabilities {
    ConnectionModelCapabilities {
        connection_id: connection_id.to_string(),
        model_id: model_id.to_string(),
        context_window: None,
        max_output_tokens: None,
        supported_efforts: Vec::new(),
        supports_thinking: false,
        supports_attachments: false,
        supports_tools: false,
        provenance: CapabilityProvenance::Unknown,
        has_manual_override: false,
    }
}

fn resolve_capabilities_with_overrides(
    connection_id: &str,
    model_id: &str,
    overrides: &BTreeMap<String, ModelCapabilityOverride>,
) -> ConnectionModelCapabilities {
    // 1. Built-in verified catalog, then prefix FamilyRules, then honest Unknown.
    let mut resolved = built_in_catalog_entry(connection_id, model_id)
        .or_else(|| family_rules_entry(connection_id, model_id))
        .unwrap_or_else(|| unknown_capabilities(connection_id, model_id));

    // 2. Zero-token Provider API Metadata merges per-field: `Some(v)` overrides
    //    lower layers (including explicit `false`), while `None` falls through to
    //    `BuiltInCatalog` / `FamilyRules`. Thin `{ id }` responses with all fields
    //    `None` do not clobber lower-layer values or provenance.
    let api_meta = {
        let guard = metadata_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .get(&(connection_id.to_string(), model_id.to_string()))
            .cloned()
    };
    if let Some(meta) = api_meta {
        if let Some(cw) = meta.context_window {
            resolved.context_window = Some(cw);
        }
        if let Some(mot) = meta.max_output_tokens {
            resolved.max_output_tokens = Some(mot);
        }
        if let Some(efforts) = &meta.supported_efforts {
            resolved.supported_efforts = normalize_efforts(efforts);
        } else if meta.supports_thinking == Some(false) {
            resolved.supported_efforts.clear();
        }
        if let Some(thinking) = meta.supports_thinking {
            resolved.supports_thinking = thinking;
        }
        if let Some(attachments) = meta.supports_attachments {
            resolved.supports_attachments = attachments;
        }
        if let Some(tools) = meta.supports_tools {
            resolved.supports_tools = tools;
        }
        if meta.has_any_field() {
            resolved.provenance = CapabilityProvenance::ApiMetadata;
        }
    }

    // 3. User Manual Override (`app_settings` / `.nexora/config.json`) strictly wins above all.
    if let Some(ov) = overrides.get(&override_map_key(connection_id, model_id)) {
        if !ov.is_empty() {
            if let Some(cw) = ov.context_window {
                resolved.context_window = Some(cw);
            }
            if let Some(mot) = ov.max_output_tokens {
                resolved.max_output_tokens = Some(mot);
            }
            if let Some(efforts) = &ov.supported_efforts {
                resolved.supported_efforts = normalize_efforts(efforts);
            }
            if let Some(thinking) = ov.supports_thinking {
                resolved.supports_thinking = thinking;
            }
            if let Some(attachments) = ov.supports_attachments {
                resolved.supports_attachments = attachments;
            }
            if let Some(tools) = ov.supports_tools {
                resolved.supports_tools = tools;
            }
            resolved.provenance = CapabilityProvenance::ManualOverride;
            resolved.has_manual_override = true;
        }
    }

    resolved
}

/// Resolve effective capabilities for `(connection_id, model_id)` using strict precedence:
/// `ManualOverride > ApiMetadata > BuiltInCatalog > Unknown`.
pub(crate) fn resolve_model_capabilities(
    db: &Database,
    connection_id: &str,
    model_id: &str,
) -> ConnectionModelCapabilities {
    let overrides = read_effective_overrides(db);
    resolve_capabilities_with_overrides(connection_id, model_id, &overrides)
}

/// Resolve effective capabilities for `(connection_id, model_id)` with an
/// optional database handle (unit tests without a DB still consult cached API
/// metadata and the built-in verified `M06` catalog).
pub(crate) fn resolve_model_capabilities_opt_db(
    db: Option<&Database>,
    connection_id: &str,
    model_id: &str,
) -> ConnectionModelCapabilities {
    let overrides = db.map_or_else(BTreeMap::new, read_effective_overrides);
    resolve_capabilities_with_overrides(connection_id, model_id, &overrides)
}

/// List effective capabilities across all supported connections & known/discovered models.
pub(crate) fn list_all_model_capabilities(db: &Database) -> Vec<ConnectionModelCapabilities> {
    let overrides = read_effective_overrides(db);
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();

    for provider in supported_providers() {
        for model in &provider.models {
            seen.insert((provider.name.clone(), model.clone()));
        }
    }
    if let Ok(compat) = CompatEndpointService::new(db).read_config() {
        if !compat.model.trim().is_empty() {
            seen.insert(("openai_compat".to_string(), compat.model));
        }
    }
    {
        let guard = metadata_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (conn, model) in guard.keys() {
            seen.insert((conn.clone(), model.clone()));
        }
    }
    for key in overrides.keys() {
        if let Some((conn, model)) = key.split_once("::") {
            if !conn.is_empty() && !model.is_empty() {
                seen.insert((conn.to_string(), model.to_string()));
            }
        }
    }

    seen.into_iter()
        .map(|(conn, model)| resolve_capabilities_with_overrides(&conn, &model, &overrides))
        .collect()
}

#[derive(Default)]
struct MetadataTarget {
    compat_url: Option<String>,
    compat_headers: Vec<(String, String)>,
}

fn compat_metadata_target(
    db: &Database,
    connection_id: &str,
) -> Result<MetadataTarget, ExecutorError> {
    if connection_id != "openai_compat" {
        return Ok(MetadataTarget::default());
    }
    let cfg = CompatEndpointService::new(db)
        .read_config()
        .map_err(|_| ExecutorError::InvalidRequest)?;
    cfg.validate().map_err(|_| ExecutorError::InvalidRequest)?;
    let mut headers = cfg.headers.clone();
    if let Some(organization) = cfg
        .organization
        .as_deref()
        .map(str::trim)
        .filter(|org| !org.is_empty())
    {
        headers.push(("OpenAI-Organization".to_string(), organization.to_string()));
    }
    Ok(MetadataTarget {
        compat_url: Some(cfg.normalized_endpoint()),
        compat_headers: headers,
    })
}

/// Fetch zero-token `/models` metadata for `connection_id`, update the process cache,
/// and return the resolved capabilities for discovered models.
pub(crate) fn refresh_connection_metadata(
    db: &Database,
    connection_id: &str,
) -> Result<Vec<ConnectionModelCapabilities>, ExecutorError> {
    refresh_connection_metadata_with_execution(db, &RequestExecutionService::new(db), connection_id)
}

fn refresh_connection_metadata_with_execution(
    db: &Database,
    execution: &RequestExecutionService<'_>,
    connection_id: &str,
) -> Result<Vec<ConnectionModelCapabilities>, ExecutorError> {
    if connection_id == crate::infrastructure::providers::chatgpt_plan::PROVIDER_NAME {
        return Err(ExecutorError::InvalidRequest);
    }
    // Validate compat configuration before any credential lookup or HTTP call.
    let target = compat_metadata_target(db, connection_id)?;
    let credential = execution
        .resolve_credential(connection_id)
        .map_err(|_| ExecutorError::Authentication)?;
    let discovered = fetch_connection_metadata(
        connection_id,
        &credential,
        target.compat_url.as_deref(),
        &target.compat_headers,
    )?;
    cache_api_metadata(connection_id, &discovered);
    let overrides = read_effective_overrides(db);
    Ok(discovered
        .into_iter()
        .map(|item| resolve_capabilities_with_overrides(connection_id, &item.model_id, &overrides))
        .collect())
}

/// Map a secret-free [`MetadataProbeError`] into `(category, hint)`.
/// Never echoes API keys, URLs with userinfo, or raw HTTP bodies.
#[must_use]
pub(crate) fn classify_probe_error(
    err: MetadataProbeError,
) -> (ConnectionTestCategory, &'static str) {
    match err {
        MetadataProbeError::AuthFailed => (
            ConnectionTestCategory::AuthFailed,
            "Authentication failed (HTTP 401/403 or missing API key). Verify your credential in Settings.",
        ),
        MetadataProbeError::Timeout => (
            ConnectionTestCategory::Timeout,
            "Connection timed out after 10 seconds. Check network connectivity, proxy settings, or endpoint availability.",
        ),
        MetadataProbeError::DnsOrProxy => (
            ConnectionTestCategory::DnsOrProxy,
            "Could not resolve host or reach endpoint through network/proxy. Check DNS and proxy settings.",
        ),
        MetadataProbeError::TlsError => (
            ConnectionTestCategory::TlsError,
            "TLS handshake or certificate verification failed. Verify HTTPS configuration and system certificates.",
        ),
        MetadataProbeError::NotRunning => (
            ConnectionTestCategory::NotRunning,
            "Connection refused. Ensure the local or remote provider server is running and listening.",
        ),
        MetadataProbeError::HttpStatus(429) => (
            ConnectionTestCategory::HttpError,
            "Provider returned HTTP 429 (rate limited). Wait briefly and retry.",
        ),
        MetadataProbeError::HttpStatus(404) => (
            ConnectionTestCategory::HttpError,
            "Provider /models endpoint returned HTTP 404. Verify the base URL path.",
        ),
        MetadataProbeError::HttpStatus(500..=599) => (
            ConnectionTestCategory::HttpError,
            "Provider returned a server error (HTTP 5xx). Retry shortly.",
        ),
        MetadataProbeError::HttpStatus(_) => (
            ConnectionTestCategory::HttpError,
            "Provider rejected the probe with an HTTP error status. Check endpoint configuration.",
        ),
        MetadataProbeError::UnexpectedResponse => (
            ConnectionTestCategory::HttpError,
            "Provider responded, but the /models payload was not valid JSON.",
        ),
        MetadataProbeError::InvalidConfig => (
            ConnectionTestCategory::HttpError,
            "Provider endpoint configuration is invalid. Use https:// (or loopback http://) with no embedded credentials.",
        ),
    }
}

fn is_loopback_http_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://localhost")
        || lower.starts_with("http://127.0.0.1")
        || lower.starts_with("http://[::1]")
}

fn resolve_test_compat_target(
    db: &Database,
    connection_id: &str,
    compat_url_override: Option<&str>,
) -> Result<MetadataTarget, MetadataProbeError> {
    if connection_id != "openai_compat" {
        return Ok(MetadataTarget::default());
    }
    if let Some(raw_url) = compat_url_override.map(str::trim).filter(|u| !u.is_empty()) {
        let mut cfg = CompatEndpointService::new(db)
            .read_config()
            .unwrap_or_else(|_| CompatConfig {
                base_url: String::new(),
                model: "probe".to_string(),
                organization: None,
                headers: Vec::new(),
                supports_tools: true,
            });
        cfg.base_url = raw_url.to_string();
        if cfg.model.trim().is_empty() {
            cfg.model = "probe".to_string();
        }
        cfg.validate()
            .map_err(|_| MetadataProbeError::InvalidConfig)?;
        let mut headers = cfg.headers.clone();
        if let Some(organization) = cfg
            .organization
            .as_deref()
            .map(str::trim)
            .filter(|org| !org.is_empty())
        {
            headers.push(("OpenAI-Organization".to_string(), organization.to_string()));
        }
        return Ok(MetadataTarget {
            compat_url: Some(cfg.normalized_endpoint()),
            compat_headers: headers,
        });
    }
    compat_metadata_target(db, connection_id).map_err(|_| MetadataProbeError::InvalidConfig)
}

/// Probe connectivity and `/models` discovery for `connection_id` with a 10s
/// timeout, returning a secret-free [`ConnectionTestResult`].
pub(crate) fn test_connection(
    db: &Database,
    connection_id: &str,
    compat_url: Option<&str>,
) -> ConnectionTestResult {
    test_connection_with_execution(
        db,
        &RequestExecutionService::new(db),
        connection_id,
        compat_url,
    )
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn test_connection_with_execution(
    db: &Database,
    execution: &RequestExecutionService<'_>,
    connection_id: &str,
    compat_url: Option<&str>,
) -> ConnectionTestResult {
    let started = Instant::now();
    if connection_id == crate::infrastructure::providers::chatgpt_plan::PROVIDER_NAME {
        return ConnectionTestResult {
            ok: false,
            latency_ms: elapsed_ms(started),
            discovered_count: 0,
            category: ConnectionTestCategory::HttpError,
            hint: "Use ChatGPT OAuth status or refresh in Settings to verify this subscription connection.".to_string(),
        };
    }

    let target = match resolve_test_compat_target(db, connection_id, compat_url) {
        Ok(target) => target,
        Err(err) => {
            let (category, hint) = classify_probe_error(err);
            return ConnectionTestResult {
                ok: false,
                latency_ms: elapsed_ms(started),
                discovered_count: 0,
                category,
                hint: hint.to_string(),
            };
        }
    };

    let credential = if let Ok(cred) = execution.resolve_credential(connection_id) {
        Some(cred)
    } else {
        let allow_anonymous = connection_id == "openai_compat"
            && target
                .compat_url
                .as_deref()
                .is_some_and(is_loopback_http_url);
        if allow_anonymous {
            None
        } else {
            let (category, hint) = classify_probe_error(MetadataProbeError::AuthFailed);
            return ConnectionTestResult {
                ok: false,
                latency_ms: elapsed_ms(started),
                discovered_count: 0,
                category,
                hint: hint.to_string(),
            };
        }
    };

    match probe_connection_metadata(
        connection_id,
        credential.as_deref(),
        target.compat_url.as_deref(),
        &target.compat_headers,
    ) {
        Ok(discovered) => {
            let count = discovered.len();
            cache_api_metadata(connection_id, &discovered);
            ConnectionTestResult {
                ok: true,
                latency_ms: elapsed_ms(started),
                discovered_count: count,
                category: ConnectionTestCategory::Ok,
                hint: format!("Connected ({count} models discovered)."),
            }
        }
        Err(err) => {
            let (category, hint) = classify_probe_error(err);
            ConnectionTestResult {
                ok: false,
                latency_ms: elapsed_ms(started),
                discovered_count: 0,
                category,
                hint: hint.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::execution::ExecutionSources;
    use crate::infrastructure::database;
    use crate::infrastructure::providers::openai::CompatConfig;
    use nexora_core::provider::{CredentialError, CredentialSource, ExecutorRegistry};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    static TEST_CACHE_MUTEX: Mutex<()> = Mutex::new(());

    fn in_memory_db() -> Database {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open memory db");
        database::configure(&conn).expect("configure memory db");
        database::migrate(&mut conn).expect("migrate memory db");
        Database::new(conn)
    }

    struct StubCredentialSource {
        secret: Option<String>,
        lookups: AtomicUsize,
    }

    impl StubCredentialSource {
        fn new(secret: Option<&str>) -> Self {
            Self {
                secret: secret.map(ToString::to_string),
                lookups: AtomicUsize::new(0),
            }
        }

        fn lookup_count(&self) -> usize {
            self.lookups.load(Ordering::SeqCst)
        }
    }

    impl CredentialSource for StubCredentialSource {
        fn exists(&self, _provider: &str) -> std::result::Result<bool, CredentialError> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            Ok(self.secret.is_some())
        }

        fn read(&self, _provider: &str) -> std::result::Result<Option<String>, CredentialError> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            Ok(self.secret.clone())
        }
    }

    #[test]
    fn capability_precedence_is_manual_override_gt_api_metadata_gt_builtin_gt_unknown() {
        // Keep the cache write separate from the model used by context statistics tests.
        let model_id = "gpt-5";
        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_api_metadata_cache();
        let db = in_memory_db();

        // 1. Unknown model -> Unknown provenance, no supported efforts.
        let unknown = resolve_model_capabilities(&db, "openai_compat", "local-custom-llama");
        assert_eq!(unknown.provenance, CapabilityProvenance::Unknown);
        assert_eq!(unknown.context_window, None);
        assert_eq!(unknown.max_output_tokens, None);
        assert!(unknown.supported_efforts.is_empty());
        assert!(!unknown.supports_thinking);

        // 2. Built-in verified catalog -> BuiltInCatalog provenance.
        let builtin = resolve_model_capabilities(&db, "gemini", "gemini-2.5-flash-lite");
        assert_eq!(builtin.provenance, CapabilityProvenance::BuiltInCatalog);
        assert_eq!(builtin.max_output_tokens, Some(65_536));
        assert!(builtin.supported_efforts.is_empty());
        assert!(!builtin.supports_thinking);

        let builtin_thinking = resolve_model_capabilities(&db, "openai", model_id);
        assert_eq!(
            builtin_thinking.provenance,
            CapabilityProvenance::BuiltInCatalog
        );
        assert_eq!(builtin_thinking.context_window, Some(400_000));
        assert_eq!(builtin_thinking.max_output_tokens, Some(128_000));
        assert!(builtin_thinking.supports_thinking);
        assert_eq!(
            builtin_thinking.supported_efforts,
            vec!["low", "medium", "high", "xhigh"]
        );

        // 3. API Metadata overrides BuiltInCatalog & Unknown.
        cache_api_metadata(
            "openai",
            &[ApiModelMetadata {
                model_id: model_id.to_string(),
                context_window: Some(512_000),
                max_output_tokens: Some(64_000),
                supported_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                supports_thinking: Some(true),
                supports_attachments: Some(true),
                supports_tools: Some(true),
            }],
        );
        let api_resolved = resolve_model_capabilities(&db, "openai", model_id);
        assert_eq!(api_resolved.provenance, CapabilityProvenance::ApiMetadata);
        assert_eq!(api_resolved.context_window, Some(512_000));
        assert_eq!(api_resolved.max_output_tokens, Some(64_000));
        assert_eq!(api_resolved.supported_efforts, vec!["low", "high"]);

        // 4. Manual Override (`app_settings`) strictly beats API Metadata and BuiltInCatalog.
        let manual = set_manual_override(
            &db,
            "openai",
            model_id,
            Some(ModelCapabilityOverride {
                context_window: Some(128_000),
                max_output_tokens: Some(16_384),
                supported_efforts: Some(vec!["medium".to_string()]),
                supports_thinking: Some(false),
                supports_attachments: None,
                supports_tools: None,
            }),
        )
        .expect("set manual override");
        assert_eq!(manual.provenance, CapabilityProvenance::ManualOverride);
        assert!(manual.has_manual_override);
        assert_eq!(manual.context_window, Some(128_000));
        assert_eq!(manual.max_output_tokens, Some(16_384));
        assert_eq!(manual.supported_efforts, vec!["medium"]);
        assert!(!manual.supports_thinking);

        // Clearing Manual Override restores ApiMetadata provenance cleanly.
        let cleared =
            set_manual_override(&db, "openai", model_id, None).expect("clear manual override");
        assert_eq!(cleared.provenance, CapabilityProvenance::ApiMetadata);
        assert!(!cleared.has_manual_override);
        assert_eq!(cleared.context_window, Some(512_000));
        assert_eq!(cleared.max_output_tokens, Some(64_000));
        clear_api_metadata_cache();
    }

    #[test]
    fn refresh_connection_metadata_rejects_invalid_openai_compat_before_credential_or_http() {
        use std::io::ErrorKind;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        listener.set_nonblocking(true).expect("set nonblocking");
        let addr = listener.local_addr().expect("local addr");

        // Case 1: unconfigured / missing URL fails validation without credential lookup.
        let empty_db = in_memory_db();
        assert!(matches!(
            refresh_connection_metadata(&empty_db, "openai_compat"),
            Err(ExecutorError::InvalidRequest)
        ));

        // Case 2: invalid URL scheme or embedded credentials fail before credential lookup or HTTP.
        for bad_url in [
            format!("ftp://{addr}/v1/chat/completions"),
            format!("https://user:secret@{addr}/v1/chat/completions"),
        ] {
            let db = in_memory_db();
            db.lock()
                .expect("db lock")
                .execute(
                    "INSERT INTO providers (name, display_name) VALUES ('openai_compat', 'OpenAI-Compatible')",
                    [],
                )
                .expect("seed provider");
            let settings = SettingsService::new(&db);
            settings
                .write("openai_compat.base_url", Some(&bad_url))
                .expect("write bad url");
            settings
                .write("openai_compat.model", Some("custom-model"))
                .expect("write model");

            assert!(matches!(
                refresh_connection_metadata(&db, "openai_compat"),
                Err(ExecutorError::InvalidRequest)
            ));

            let creds = Arc::new(StubCredentialSource::new(Some("sk-compat-secret")));
            let sources = ExecutionSources::with_test_sources(
                Arc::new(ExecutorRegistry::new()),
                Arc::clone(&creds) as Arc<dyn CredentialSource>,
            );
            let execution = RequestExecutionService::with_sources(&db, sources);
            let res = refresh_connection_metadata_with_execution(&db, &execution, "openai_compat");
            assert!(matches!(res, Err(ExecutorError::InvalidRequest)));
            assert_eq!(creds.lookup_count(), 0);
            assert!(matches!(listener.accept(), Err(ref e) if e.kind() == ErrorKind::WouldBlock));
        }

        // Case 3: reserved header fails validation before credential lookup or HTTP.
        let db = in_memory_db();
        db.lock()
            .expect("db lock")
            .execute(
                "INSERT INTO providers (name, display_name) VALUES ('openai_compat', 'OpenAI-Compatible')",
                [],
            )
            .expect("seed provider");
        let settings = SettingsService::new(&db);
        settings
            .write(
                "openai_compat.base_url",
                Some(&format!("http://{addr}/v1/chat/completions")),
            )
            .expect("write valid url");
        settings
            .write("openai_compat.model", Some("custom-model"))
            .expect("write model");
        settings
            .write(
                "openai_compat.headers_json",
                Some(r#"[["Authorization","Bearer overridden"]]"#),
            )
            .expect("write reserved header");

        let creds = Arc::new(StubCredentialSource::new(Some("sk-compat-secret")));
        let sources = ExecutionSources::with_test_sources(
            Arc::new(ExecutorRegistry::new()),
            Arc::clone(&creds) as Arc<dyn CredentialSource>,
        );
        let execution = RequestExecutionService::with_sources(&db, sources);
        let res = refresh_connection_metadata_with_execution(&db, &execution, "openai_compat");
        assert!(matches!(res, Err(ExecutorError::InvalidRequest)));
        assert_eq!(creds.lookup_count(), 0);
        assert!(matches!(listener.accept(), Err(ref e) if e.kind() == ErrorKind::WouldBlock));
    }

    #[test]
    fn refresh_connection_metadata_forwards_openai_compat_organization_header() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_api_metadata_cache();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let server = std::thread::spawn(move || -> String {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let body = r#"{"data":[{"id":"discovered-llama","context_length":65536}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).expect("write response");
            stream.flush().expect("flush response");
            String::from_utf8_lossy(&raw).into_owned()
        });

        let db = in_memory_db();
        db.lock()
            .expect("db lock")
            .execute(
                "INSERT INTO providers (name, display_name) VALUES ('openai_compat', 'OpenAI-Compatible')",
                [],
            )
            .expect("seed provider");
        CompatEndpointService::new(&db)
            .write_config(&CompatConfig {
                base_url: format!("http://{addr}/v1/chat/completions"),
                model: "compat-default".to_string(),
                organization: Some("org-compat-42".to_string()),
                headers: vec![("X-Custom".to_string(), "hdr-val".to_string())],
                supports_tools: true,
            })
            .expect("write compat config");

        let creds = Arc::new(StubCredentialSource::new(Some("sk-compat-secret")));
        let sources = ExecutionSources::with_test_sources(
            Arc::new(ExecutorRegistry::new()),
            Arc::clone(&creds) as Arc<dyn CredentialSource>,
        );
        let execution = RequestExecutionService::with_sources(&db, sources);
        let caps = refresh_connection_metadata_with_execution(&db, &execution, "openai_compat")
            .expect("refresh compat metadata succeeds");
        let head = server.join().expect("server thread joins");

        assert!(
            head.starts_with("GET /v1/models "),
            "unexpected req: {head}"
        );
        let has_header = |name: &str, value: &str| {
            head.lines().any(|line| {
                line.split_once(':')
                    .is_some_and(|(k, v)| k.eq_ignore_ascii_case(name) && v.trim() == value)
            })
        };
        assert!(has_header("Authorization", "Bearer sk-compat-secret"));
        assert!(has_header("OpenAI-Organization", "org-compat-42"));
        assert!(has_header("X-Custom", "hdr-val"));
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].connection_id, "openai_compat");
        assert_eq!(caps[0].model_id, "discovered-llama");
        assert_eq!(caps[0].context_window, Some(65_536));
        assert_eq!(caps[0].provenance, CapabilityProvenance::ApiMetadata);
        clear_api_metadata_cache();
    }

    const EXPECTED_PRESET_BASELINES: &[(&str, &str, u32, u32, bool, bool, bool)] = &[
        (
            "deepseek",
            "deepseek-flash",
            128_000,
            8_192,
            false,
            false,
            true,
        ),
        (
            "deepseek",
            "deepseek-v4-pro",
            128_000,
            64_000,
            true,
            false,
            true,
        ),
        ("qwen", "qwen-max", 128_000, 8_192, false, false, true),
        ("qwen", "qwq-plus", 131_072, 16_384, true, false, true),
        ("qwen", "qwen-vl-max", 128_000, 8_192, false, true, true),
        ("zai", "glm-4.5", 128_000, 96_000, true, false, true),
        (
            "minimax",
            "MiniMax-M1",
            1_000_000,
            40_000,
            true,
            false,
            true,
        ),
        (
            "moonshot",
            "kimi-thinking-preview",
            128_000,
            16_384,
            true,
            false,
            true,
        ),
        (
            "moonshot",
            "moonshot-v1-128k-vision-preview",
            128_000,
            8_192,
            false,
            true,
            true,
        ),
        (
            "groq",
            "llama-3.3-70b-versatile",
            131_072,
            32_768,
            false,
            false,
            true,
        ),
        (
            "groq",
            "openai/gpt-oss-120b",
            131_072,
            65_536,
            true,
            false,
            true,
        ),
        ("xai", "grok-3-mini", 131_072, 32_768, true, false, true),
        ("xai", "grok-4.7", 256_000, 65_536, true, true, true),
        (
            "mistral",
            "codestral-latest",
            256_000,
            32_768,
            false,
            false,
            true,
        ),
        (
            "mistral",
            "magistral-medium-latest",
            128_000,
            40_960,
            true,
            false,
            true,
        ),
        (
            "mistral",
            "pixtral-large-latest",
            131_072,
            16_384,
            false,
            true,
            true,
        ),
    ];

    #[test]
    fn all_eight_new_presets_have_verified_baseline_capabilities() {
        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_api_metadata_cache();
        let db = in_memory_db();

        for &(conn, model, window, max_out, thinking, attachments, tools) in
            EXPECTED_PRESET_BASELINES
        {
            let resolved = resolve_model_capabilities(&db, conn, model);
            assert_eq!(resolved.provenance, CapabilityProvenance::BuiltInCatalog);
            assert_eq!(resolved.context_window, Some(window), "{conn}/{model}");
            assert_eq!(resolved.max_output_tokens, Some(max_out), "{conn}/{model}");
            assert_eq!(resolved.supports_thinking, thinking, "{conn}/{model}");
            assert_eq!(resolved.supports_attachments, attachments, "{conn}/{model}");
            assert_eq!(resolved.supports_tools, tools, "{conn}/{model}");
            assert_eq!(
                resolved.supported_efforts.is_empty(),
                !thinking,
                "{conn}/{model} efforts"
            );
        }
    }

    #[test]
    fn new_preset_respects_manual_override_gt_api_metadata_gt_builtin() {
        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_api_metadata_cache();
        let db = in_memory_db();

        // Verify ManualOverride > ApiMetadata > BuiltInCatalog precedence on a new preset.
        cache_api_metadata(
            "deepseek",
            &[ApiModelMetadata {
                model_id: "deepseek-v4-pro".to_string(),
                context_window: Some(256_000),
                max_output_tokens: Some(32_000),
                supported_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                supports_thinking: Some(true),
                supports_attachments: Some(false),
                supports_tools: Some(true),
            }],
        );
        let from_api = resolve_model_capabilities(&db, "deepseek", "deepseek-v4-pro");
        assert_eq!(from_api.provenance, CapabilityProvenance::ApiMetadata);
        assert_eq!(from_api.context_window, Some(256_000));
        assert_eq!(from_api.max_output_tokens, Some(32_000));

        set_manual_override(
            &db,
            "deepseek",
            "deepseek-v4-pro",
            Some(ModelCapabilityOverride {
                context_window: Some(64_000),
                max_output_tokens: None,
                supported_efforts: Some(vec!["medium".to_string()]),
                supports_thinking: Some(true),
                supports_attachments: Some(false),
                supports_tools: Some(true),
            }),
        )
        .expect("set manual override");
        let from_override = resolve_model_capabilities(&db, "deepseek", "deepseek-v4-pro");
        assert_eq!(
            from_override.provenance,
            CapabilityProvenance::ManualOverride
        );
        assert_eq!(from_override.context_window, Some(64_000));
        // Unspecified override field falls through from ApiMetadata.
        assert_eq!(from_override.max_output_tokens, Some(32_000));
        assert_eq!(from_override.supported_efforts, vec!["medium".to_string()]);
        clear_api_metadata_cache();
    }

    #[test]
    fn project_capability_override_precedes_personal_and_scoped_clear_falls_back() {
        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_api_metadata_cache();
        let db = in_memory_db();
        let scratch_base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("nexora-test-tmp");
        std::fs::create_dir_all(&scratch_base).expect("mkdir scratch");
        let ws = scratch_base.join(format!("stage4-caps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).expect("create temp ws");
        let canon_ws =
            crate::application::workspace::validate_workspace_root(&ws.to_string_lossy())
                .expect("valid workspace root");
        crate::application::settings::SettingsService::new(&db)
            .write(
                nexora_core::config::WORKSPACE_ROOT_KEY,
                Some(&canon_ws.to_string_lossy()),
            )
            .expect("persist workspace root");

        // Personal override = 32_000 context window.
        set_scoped_manual_override(
            &db,
            crate::application::settings::SettingScope::Personal,
            "openai",
            "gpt-5.2",
            Some(ModelCapabilityOverride {
                context_window: Some(32_000),
                max_output_tokens: None,
                supported_efforts: Some(vec!["low".to_string()]),
                supports_thinking: Some(false),
                supports_attachments: Some(true),
                supports_tools: Some(true),
            }),
        )
        .expect("set personal override");

        // Project override = 64_000 context window takes precedence over Personal.
        let proj = set_scoped_manual_override(
            &db,
            crate::application::settings::SettingScope::Project,
            "openai",
            "gpt-5.2",
            Some(ModelCapabilityOverride {
                context_window: Some(64_000),
                max_output_tokens: Some(16_384),
                supported_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                supports_thinking: Some(true),
                supports_attachments: Some(true),
                supports_tools: Some(true),
            }),
        )
        .expect("set project override");
        assert_eq!(proj.context_window, Some(64_000));
        assert_eq!(proj.max_output_tokens, Some(16_384));
        assert_eq!(proj.provenance, CapabilityProvenance::ManualOverride);

        // Clearing the Project override falls back cleanly to Personal 32_000.
        let cleared = set_scoped_manual_override(
            &db,
            crate::application::settings::SettingScope::Project,
            "openai",
            "gpt-5.2",
            None,
        )
        .expect("clear project override");
        assert_eq!(cleared.context_window, Some(32_000));
        assert_eq!(cleared.provenance, CapabilityProvenance::ManualOverride);

        let _ = std::fs::remove_dir_all(&ws);
        clear_api_metadata_cache();
    }

    #[test]
    fn thin_api_metadata_falls_through_per_field_to_builtin_and_family_rules() {
        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_api_metadata_cache();
        let db = in_memory_db();

        // Thin /v1/models entry (all None) for an exact catalog model (`gpt-4o`)
        // must NOT overwrite BuiltInCatalog fields or provenance.
        cache_api_metadata(
            "openai",
            &[
                ApiModelMetadata {
                    model_id: "gpt-4o".to_string(),
                    context_window: None,
                    max_output_tokens: None,
                    supported_efforts: None,
                    supports_thinking: None,
                    supports_attachments: None,
                    supports_tools: None,
                },
                // Thin entry for an unlisted dated variant resolved via FamilyRules.
                ApiModelMetadata {
                    model_id: "gpt-4o-2024-11-20".to_string(),
                    context_window: None,
                    max_output_tokens: None,
                    supported_efforts: None,
                    supports_thinking: None,
                    supports_attachments: None,
                    supports_tools: None,
                },
                // Partial API metadata supplies context_window only; remaining
                // fields fall through to FamilyRules while provenance upgrades to ApiMetadata.
                ApiModelMetadata {
                    model_id: "claude-sonnet-4-20260101".to_string(),
                    context_window: Some(1_000_000),
                    max_output_tokens: None,
                    supported_efforts: None,
                    supports_thinking: None,
                    supports_attachments: None,
                    supports_tools: None,
                },
            ],
        );

        let exact = resolve_model_capabilities(&db, "openai", "gpt-4o");
        assert_eq!(exact.provenance, CapabilityProvenance::BuiltInCatalog);
        assert_eq!(exact.context_window, Some(128_000));
        assert_eq!(exact.max_output_tokens, Some(16_384));
        assert!(exact.supports_tools);
        assert!(exact.supports_attachments);
        assert!(!exact.supports_thinking);

        let dated = resolve_model_capabilities(&db, "openai", "gpt-4o-2024-11-20");
        assert_eq!(dated.provenance, CapabilityProvenance::BuiltInCatalog);
        assert_eq!(dated.context_window, Some(128_000));
        assert_eq!(dated.max_output_tokens, Some(16_384));
        assert!(dated.supports_tools);
        assert!(dated.supports_attachments);

        let partial = resolve_model_capabilities(&db, "openai", "claude-sonnet-4-20260101");
        assert_eq!(partial.provenance, CapabilityProvenance::ApiMetadata);
        assert_eq!(partial.context_window, Some(1_000_000));
        assert_eq!(partial.max_output_tokens, Some(64_000));
        assert!(partial.supports_thinking);
        assert!(partial.supports_tools);
        assert!(partial.supports_attachments);
        assert_eq!(
            partial.supported_efforts,
            vec!["low", "medium", "high", "xhigh"]
        );

        // Legacy DeepSeek aliases stay covered by FamilyRules.
        let legacy_ds = resolve_model_capabilities(&db, "deepseek", "deepseek-reasoner");
        assert_eq!(legacy_ds.provenance, CapabilityProvenance::BuiltInCatalog);
        assert_eq!(legacy_ds.context_window, Some(128_000));
        assert_eq!(legacy_ds.max_output_tokens, Some(64_000));
        assert!(legacy_ds.supports_thinking);

        clear_api_metadata_cache();
    }

    #[test]
    fn family_rules_never_override_explicit_api_false_or_manual_override() {
        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_api_metadata_cache();
        let db = in_memory_db();

        // FamilyRules would say `gpt-5*` supports tools=true, thinking=true, attachments=true.
        // Explicit API `false` must strictly win over FamilyRules.
        cache_api_metadata(
            "openai",
            &[ApiModelMetadata {
                model_id: "gpt-5-restricted".to_string(),
                context_window: None,
                max_output_tokens: None,
                supported_efforts: None,
                supports_thinking: Some(false),
                supports_attachments: Some(false),
                supports_tools: Some(false),
            }],
        );
        let api_false = resolve_model_capabilities(&db, "openai", "gpt-5-restricted");
        assert_eq!(api_false.provenance, CapabilityProvenance::ApiMetadata);
        // Omitted fields fall through to FamilyRules...
        assert_eq!(api_false.context_window, Some(400_000));
        assert_eq!(api_false.max_output_tokens, Some(128_000));
        // ...while explicit false is never overridden by FamilyRules, and efforts clear.
        assert!(!api_false.supports_thinking);
        assert!(api_false.supported_efforts.is_empty());
        assert!(!api_false.supports_attachments);
        assert!(!api_false.supports_tools);

        // ManualOverride strictly beats API false.
        let manual = set_manual_override(
            &db,
            "openai",
            "gpt-5-restricted",
            Some(ModelCapabilityOverride {
                supports_tools: Some(true),
                ..ModelCapabilityOverride::default()
            }),
        )
        .expect("manual override");
        assert_eq!(manual.provenance, CapabilityProvenance::ManualOverride);
        assert!(manual.supports_tools);
        assert!(!manual.supports_thinking);

        clear_api_metadata_cache();
    }

    #[test]
    fn test_connection_classifies_all_error_categories() {
        use crate::infrastructure::providers::transport::{
            classify_probe_error_details, TransportProbeError,
        };
        use std::io::ErrorKind;

        assert_eq!(
            classify_probe_error_details(true, false, None, ""),
            TransportProbeError::Timeout
        );
        assert_eq!(
            classify_probe_error_details(false, true, Some(ErrorKind::ConnectionRefused), ""),
            TransportProbeError::NotRunning
        );
        assert_eq!(
            classify_probe_error_details(false, true, None, "rustls invalid peer certificate"),
            TransportProbeError::TlsError
        );
        assert_eq!(
            classify_probe_error_details(false, true, None, "failed to lookup address information"),
            TransportProbeError::DnsOrProxy
        );

        for (err, expected) in [
            (
                MetadataProbeError::AuthFailed,
                ConnectionTestCategory::AuthFailed,
            ),
            (MetadataProbeError::Timeout, ConnectionTestCategory::Timeout),
            (
                MetadataProbeError::DnsOrProxy,
                ConnectionTestCategory::DnsOrProxy,
            ),
            (
                MetadataProbeError::TlsError,
                ConnectionTestCategory::TlsError,
            ),
            (
                MetadataProbeError::NotRunning,
                ConnectionTestCategory::NotRunning,
            ),
            (
                MetadataProbeError::HttpStatus(502),
                ConnectionTestCategory::HttpError,
            ),
            (
                MetadataProbeError::UnexpectedResponse,
                ConnectionTestCategory::HttpError,
            ),
            (
                MetadataProbeError::InvalidConfig,
                ConnectionTestCategory::HttpError,
            ),
        ] {
            assert_eq!(classify_probe_error(err).0, expected);
        }
    }

    #[test]
    fn test_connection_loopback_probe_and_secret_free_hints() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_api_metadata_cache();

        let db = in_memory_db();
        let creds = Arc::new(StubCredentialSource::new(Some("sk-top-secret-key-999")));
        let sources = ExecutionSources::with_test_sources(
            Arc::new(ExecutorRegistry::new()),
            Arc::clone(&creds) as Arc<dyn CredentialSource>,
        );
        let execution = RequestExecutionService::with_sources(&db, sources);

        // Embedded userinfo URL is rejected without echoing URL or credentials.
        let bad = test_connection_with_execution(
            &db,
            &execution,
            "openai_compat",
            Some("https://alice:supersecret@example.com/v1"),
        );
        assert!(!bad.ok);
        assert_eq!(bad.category, ConnectionTestCategory::HttpError);
        assert!(!bad.hint.contains("supersecret"));
        assert!(!bad.hint.contains("alice"));
        assert!(!bad.hint.contains("sk-top-secret-key-999"));

        // Live loopback 200 OK returns ok=true + discovered_count, and 401 returns
        // auth_failed without echoing raw HTTP body or secret.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let server = std::thread::spawn(move || {
            for idx in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let _ = stream.read(&mut buf);
                let (status_line, body) = if idx == 0 {
                    (
                        "HTTP/1.1 200 OK",
                        r#"{"data":[{"id":"gpt-5"},{"id":"gpt-4o"}]}"#,
                    )
                } else {
                    (
                        "HTTP/1.1 401 Unauthorized",
                        r#"{"error":"RAW_LEAK_MARKER_401"}"#,
                    )
                };
                let resp = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });

        let ok_res = test_connection_with_execution(
            &db,
            &execution,
            "openai_compat",
            Some(&format!("http://{addr}/v1")),
        );
        assert!(ok_res.ok);
        assert_eq!(ok_res.category, ConnectionTestCategory::Ok);
        assert_eq!(ok_res.discovered_count, 2);

        let auth_res = test_connection_with_execution(
            &db,
            &execution,
            "openai_compat",
            Some(&format!("http://{addr}/v1")),
        );
        assert!(!auth_res.ok);
        assert_eq!(auth_res.category, ConnectionTestCategory::AuthFailed);
        assert_eq!(auth_res.discovered_count, 0);
        assert!(!auth_res.hint.contains("RAW_LEAK_MARKER_401"));
        assert!(!auth_res.hint.contains("sk-top-secret-key-999"));

        server.join().expect("server thread joins");
        clear_api_metadata_cache();
    }
}
