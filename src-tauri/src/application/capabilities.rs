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

use std::sync::Arc;

use crate::infrastructure::database::Database;
use crate::infrastructure::providers::async_spawner::AsyncSpawner;
use crate::infrastructure::providers::catalog::gemini::GEMINI_LITE_MODELS;
use crate::infrastructure::providers::metadata::{fetch_connection_metadata, ApiModelMetadata};
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

fn built_in_catalog_entry(
    connection_id: &str,
    model_id: &str,
) -> Option<ConnectionModelCapabilities> {
    if let Some(spec) = crate::infrastructure::providers::catalog::openai::find_preset_model_spec(
        connection_id,
        model_id,
    ) {
        return Some(ConnectionModelCapabilities {
            connection_id: connection_id.to_string(),
            model_id: model_id.to_string(),
            context_window: Some(spec.context_window),
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
        });
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
    spawner: Arc<dyn AsyncSpawner>,
) -> Result<Vec<ConnectionModelCapabilities>, ExecutorError> {
    refresh_connection_metadata_with_execution(
        db,
        &RequestExecutionService::new(db),
        connection_id,
        spawner,
    )
}

fn refresh_connection_metadata_with_execution(
    db: &Database,
    execution: &RequestExecutionService<'_>,
    connection_id: &str,
    spawner: Arc<dyn AsyncSpawner>,
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
        spawner,
    )?;
    cache_api_metadata(connection_id, &discovered);
    let overrides = read_effective_overrides(db);
    Ok(discovered
        .into_iter()
        .map(|item| resolve_capabilities_with_overrides(connection_id, &item.model_id, &overrides))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::execution::ExecutionSources;
    use crate::infrastructure::database;
    use crate::infrastructure::providers::async_spawner::test_spawner;
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
        let model_id = "gpt-5.6-sol";
        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

        let builtin_thinking = resolve_model_capabilities(&db, "openai", model_id);
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
                model_id: model_id.to_string(),
                context_window: Some(512_000),
                supported_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                supports_thinking: Some(true),
                supports_attachments: Some(true),
                supports_tools: Some(true),
            }],
        );
        let api_resolved = resolve_model_capabilities(&db, "openai", model_id);
        assert_eq!(api_resolved.provenance, CapabilityProvenance::ApiMetadata);
        assert_eq!(api_resolved.context_window, Some(512_000));
        assert_eq!(api_resolved.supported_efforts, vec!["low", "high"]);

        // 4. Manual Override (`app_settings`) strictly beats API Metadata and BuiltInCatalog.
        let manual = set_manual_override(
            &db,
            "openai",
            model_id,
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
        let cleared =
            set_manual_override(&db, "openai", model_id, None).expect("clear manual override");
        assert_eq!(cleared.provenance, CapabilityProvenance::ApiMetadata);
        assert!(!cleared.has_manual_override);
        assert_eq!(cleared.context_window, Some(512_000));
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
            refresh_connection_metadata(&empty_db, "openai_compat", test_spawner()),
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
                refresh_connection_metadata(&db, "openai_compat", test_spawner()),
                Err(ExecutorError::InvalidRequest)
            ));

            let creds = Arc::new(StubCredentialSource::new(Some("sk-compat-secret")));
            let sources = ExecutionSources::with_test_sources(
                Arc::new(ExecutorRegistry::new()),
                Arc::clone(&creds) as Arc<dyn CredentialSource>,
            );
            let execution = RequestExecutionService::with_sources(&db, sources);
            let res = refresh_connection_metadata_with_execution(
                &db,
                &execution,
                "openai_compat",
                test_spawner(),
            );
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
        let res = refresh_connection_metadata_with_execution(
            &db,
            &execution,
            "openai_compat",
            test_spawner(),
        );
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
        let caps = refresh_connection_metadata_with_execution(
            &db,
            &execution,
            "openai_compat",
            test_spawner(),
        )
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

    #[test]
    fn all_eight_new_presets_have_verified_baseline_capabilities() {
        let _guard = TEST_CACHE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_api_metadata_cache();
        let db = in_memory_db();

        let expected: &[(&str, &str, u32, bool, bool, bool)] = &[
            ("deepseek", "deepseek-chat", 128_000, false, false, true),
            ("deepseek", "deepseek-reasoner", 128_000, true, false, true),
            ("qwen", "qwen-max", 128_000, false, false, true),
            ("qwen", "qwq-plus", 131_072, true, false, true),
            ("qwen", "qwen-vl-max", 128_000, false, true, true),
            ("zai", "glm-4.5", 128_000, true, false, true),
            ("minimax", "MiniMax-M1", 1_000_000, true, false, true),
            (
                "moonshot",
                "kimi-thinking-preview",
                128_000,
                true,
                false,
                true,
            ),
            (
                "moonshot",
                "moonshot-v1-128k-vision-preview",
                128_000,
                false,
                true,
                true,
            ),
            (
                "groq",
                "llama-3.3-70b-versatile",
                131_072,
                false,
                false,
                true,
            ),
            (
                "groq",
                "deepseek-r1-distill-llama-70b",
                131_072,
                true,
                false,
                true,
            ),
            ("xai", "grok-3-mini", 131_072, true, false, true),
            ("xai", "grok-2-vision-1212", 32_768, false, true, true),
            ("mistral", "codestral-latest", 256_000, false, false, true),
            (
                "mistral",
                "magistral-medium-latest",
                128_000,
                true,
                false,
                true,
            ),
            (
                "mistral",
                "pixtral-large-latest",
                131_072,
                false,
                true,
                true,
            ),
        ];

        for &(conn, model, window, thinking, attachments, tools) in expected {
            let resolved = resolve_model_capabilities(&db, conn, model);
            assert_eq!(resolved.provenance, CapabilityProvenance::BuiltInCatalog);
            assert_eq!(resolved.context_window, Some(window), "{conn}/{model}");
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
                model_id: "deepseek-reasoner".to_string(),
                context_window: Some(256_000),
                supported_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                supports_thinking: Some(true),
                supports_attachments: Some(false),
                supports_tools: Some(true),
            }],
        );
        let from_api = resolve_model_capabilities(&db, "deepseek", "deepseek-reasoner");
        assert_eq!(from_api.provenance, CapabilityProvenance::ApiMetadata);
        assert_eq!(from_api.context_window, Some(256_000));

        set_manual_override(
            &db,
            "deepseek",
            "deepseek-reasoner",
            Some(ModelCapabilityOverride {
                context_window: Some(64_000),
                supported_efforts: Some(vec!["medium".to_string()]),
                supports_thinking: Some(true),
                supports_attachments: Some(false),
                supports_tools: Some(true),
            }),
        )
        .expect("set manual override");
        let from_override = resolve_model_capabilities(&db, "deepseek", "deepseek-reasoner");
        assert_eq!(
            from_override.provenance,
            CapabilityProvenance::ManualOverride
        );
        assert_eq!(from_override.context_window, Some(64_000));
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
                supported_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                supports_thinking: Some(true),
                supports_attachments: Some(true),
                supports_tools: Some(true),
            }),
        )
        .expect("set project override");
        assert_eq!(proj.context_window, Some(64_000));
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
}
