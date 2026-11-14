//! `(connection_id, model_id)` Capability Catalog & Provenance (`M06` / P10, P11).
//!
//! Enforces strict precedence:
//! `User Manual Override (app_settings)` > `Provider API Metadata (zero-token /models)`
//! > `Built-in Verified Catalog` > `Unknown`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

use nexora_core::config::MODEL_CAPABILITY_OVERRIDES_KEY;
use nexora_core::provider::{ExecutorError, ReasoningEffort};
use serde::{Deserialize, Serialize};

use crate::infrastructure::database::Database;
use crate::infrastructure::providers::catalog::gemini::GEMINI_LITE_MODELS;
use crate::infrastructure::providers::metadata::{fetch_connection_metadata, ApiModelMetadata};
use crate::infrastructure::providers::supported_providers;

use super::compat::CompatEndpointService;
use super::context_stats::context_limit_for;
use super::execution::RequestExecutionService;
use super::settings::SettingsService;

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
            && self.supported_efforts.is_none()
            && self.supports_thinking.is_none()
            && self.supports_attachments.is_none()
            && self.supports_tools.is_none()
    }
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
    Ok(resolve_capabilities_with_overrides(
        connection_id,
        model_id,
        &map,
    ))
}

fn built_in_catalog_entry(
    connection_id: &str,
    model_id: &str,
) -> Option<ConnectionModelCapabilities> {
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
            "xkiro" | "openrouter" | "nvidia" | "opencode_zen" => {
                let thinking_capable = model_id.contains("r1")
                    || model_id.contains("claude")
                    || model_id.contains("gpt-5")
                    || model_id.contains("qwen3")
                    || model_id.contains("gemini");
                let efforts = if thinking_capable {
                    vec!["low".to_string(), "medium".to_string(), "high".to_string()]
                } else {
                    Vec::new()
                };
                (efforts, thinking_capable, true, true)
            }
            _ => (Vec::new(), false, false, true),
        };

    Some(ConnectionModelCapabilities {
        connection_id: connection_id.to_string(),
        model_id: model_id.to_string(),
        context_window,
        supported_efforts,
        supports_thinking,
        supports_attachments,
        supports_tools,
        provenance: CapabilityProvenance::BuiltInCatalog,
        has_manual_override: false,
    })
}

fn unknown_capabilities(connection_id: &str, model_id: &str) -> ConnectionModelCapabilities {
    ConnectionModelCapabilities {
        connection_id: connection_id.to_string(),
        model_id: model_id.to_string(),
        context_window: None,
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
    // 1. Built-in verified catalog or Unknown.
    let mut resolved = built_in_catalog_entry(connection_id, model_id)
        .unwrap_or_else(|| unknown_capabilities(connection_id, model_id));

    // 2. Zero-token Provider API Metadata overrides built-in/unknown where specified.
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
        if let Some(efforts) = meta.supported_efforts {
            resolved.supported_efforts = normalize_efforts(&efforts);
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
        resolved.provenance = CapabilityProvenance::ApiMetadata;
    }

    // 3. User Manual Override (`app_settings`) strictly wins above all.
    if let Some(ov) = overrides.get(&override_map_key(connection_id, model_id)) {
        if !ov.is_empty() {
            if let Some(cw) = ov.context_window {
                resolved.context_window = Some(cw);
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
    let overrides = read_manual_overrides(db).unwrap_or_default();
    resolve_capabilities_with_overrides(connection_id, model_id, &overrides)
}

/// List effective capabilities across all supported connections & known/discovered models.
pub(crate) fn list_all_model_capabilities(db: &Database) -> Vec<ConnectionModelCapabilities> {
    let overrides = read_manual_overrides(db).unwrap_or_default();
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

/// Fetch zero-token `/models` metadata for `connection_id`, update the process cache,
/// and return the resolved capabilities for discovered models.
pub(crate) fn refresh_connection_metadata(
    db: &Database,
    connection_id: &str,
) -> Result<Vec<ConnectionModelCapabilities>, ExecutorError> {
    let credential = RequestExecutionService::new(db)
        .resolve_credential(connection_id)
        .map_err(|_| ExecutorError::Authentication)?;
    let (compat_url, compat_headers) = if connection_id == "openai_compat" {
        let cfg = CompatEndpointService::new(db)
            .read_config()
            .map_err(|_| ExecutorError::InvalidRequest)?;
        (Some(cfg.base_url), cfg.headers)
    } else {
        (None, Vec::new())
    };
    let discovered = fetch_connection_metadata(
        connection_id,
        &credential,
        compat_url.as_deref(),
        &compat_headers,
    )?;
    cache_api_metadata(connection_id, &discovered);
    let overrides = read_manual_overrides(db).unwrap_or_default();
    Ok(discovered
        .into_iter()
        .map(|item| resolve_capabilities_with_overrides(connection_id, &item.model_id, &overrides))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database;

    fn in_memory_db() -> Database {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open memory db");
        database::configure(&conn).expect("configure memory db");
        database::migrate(&mut conn).expect("migrate memory db");
        Database::new(conn)
    }

    #[test]
    fn capability_precedence_is_manual_override_gt_api_metadata_gt_builtin_gt_unknown() {
        clear_api_metadata_cache();
        let db = in_memory_db();

        // 1. Unknown model -> Unknown provenance, no supported efforts.
        let unknown = resolve_model_capabilities(&db, "openai_compat", "local-custom-llama");
        assert_eq!(unknown.provenance, CapabilityProvenance::Unknown);
        assert_eq!(unknown.context_window, None);
        assert!(unknown.supported_efforts.is_empty());
        assert!(!unknown.supports_thinking);

        // 2. Built-in verified catalog -> BuiltInCatalog provenance.
        let builtin = resolve_model_capabilities(&db, "gemini", "gemini-3.1-flash-lite");
        assert_eq!(builtin.provenance, CapabilityProvenance::BuiltInCatalog);
        assert!(builtin.supported_efforts.is_empty());
        assert!(!builtin.supports_thinking);

        let builtin_thinking = resolve_model_capabilities(&db, "openai", "gpt-5.6-terra");
        assert_eq!(
            builtin_thinking.provenance,
            CapabilityProvenance::BuiltInCatalog
        );
        assert!(builtin_thinking.supports_thinking);
        assert_eq!(
            builtin_thinking.supported_efforts,
            vec!["low", "medium", "high", "xhigh"]
        );

        // 3. API Metadata overrides BuiltInCatalog & Unknown.
        cache_api_metadata(
            "openai",
            &[ApiModelMetadata {
                model_id: "gpt-5.6-terra".to_string(),
                context_window: Some(512_000),
                supported_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                supports_thinking: Some(true),
                supports_attachments: Some(true),
                supports_tools: Some(true),
            }],
        );
        let api_resolved = resolve_model_capabilities(&db, "openai", "gpt-5.6-terra");
        assert_eq!(api_resolved.provenance, CapabilityProvenance::ApiMetadata);
        assert_eq!(api_resolved.context_window, Some(512_000));
        assert_eq!(api_resolved.supported_efforts, vec!["low", "high"]);

        // 4. Manual Override (`app_settings`) strictly beats API Metadata and BuiltInCatalog.
        let manual = set_manual_override(
            &db,
            "openai",
            "gpt-5.6-terra",
            Some(ModelCapabilityOverride {
                context_window: Some(128_000),
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
        assert_eq!(manual.supported_efforts, vec!["medium"]);
        assert!(!manual.supports_thinking);

        // Clearing Manual Override restores ApiMetadata provenance cleanly.
        let cleared = set_manual_override(&db, "openai", "gpt-5.6-terra", None)
            .expect("clear manual override");
        assert_eq!(cleared.provenance, CapabilityProvenance::ApiMetadata);
        assert!(!cleared.has_manual_override);
        assert_eq!(cleared.context_window, Some(512_000));
        clear_api_metadata_cache();
    }
}
