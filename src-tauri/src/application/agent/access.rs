//! Server-owned, live security ceilings and secret-free execution identity.
use super::permissions::PermissionStore;
use super::tools::path_safety::{canonical_root, path_to_comparable_string, tool_scope};
use crate::application::execution::ToolCall;
use crate::application::settings::SettingsService;
use crate::infrastructure::database::Database;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

pub(crate) const PROFILE_KEY: &str = "agent.access_profile";
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccessProfile {
    Standard,
    Supervised,
    ReadOnly,
}
impl AccessProfile {
    pub(crate) fn resolve(value: Option<&str>) -> Self {
        match value {
            None | Some("standard") => Self::Standard,
            Some("supervised") => Self::Supervised,
            _ => Self::ReadOnly,
        }
    }
}
/// Monotonic, process-local revocation clock. Keyed by the shared connection,
/// never a path or credential. Weak entries cannot resurrect a prior database.
type Clock = Arc<RwLock<u64>>;
pub(crate) fn policy_clock(db: &Database) -> Clock {
    static CLOCKS: OnceLock<Mutex<HashMap<usize, Weak<RwLock<u64>>>>> = OnceLock::new();
    let key = match db.lock() {
        Ok(conn) => std::ptr::from_ref(&*conn) as usize,
        Err(_) => return Arc::new(RwLock::new(u64::MAX)),
    };
    let mut clocks = CLOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(clock) = clocks.get(&key).and_then(Weak::upgrade) {
        return clock;
    }
    clocks.retain(|_, clock| clock.strong_count() > 0);
    let clock = Arc::new(RwLock::new(0));
    clocks.insert(key, Arc::downgrade(&clock));
    clock
}
struct ContextState {
    db: Option<Database>,
    clock: Clock,
    selected: AccessProfile,
    parent: Option<AccessContext>,
    child_ceiling: AccessProfile,
    plan: AtomicBool,
    revision: AtomicU64,
    environment: Arc<Mutex<(String, u64)>>,
    #[cfg(test)]
    environment_override: Arc<Mutex<Option<String>>>,
}
#[derive(Clone)]
pub(crate) struct AccessContext(Arc<ContextState>);
impl std::fmt::Debug for AccessContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessContext").finish_non_exhaustive()
    }
}
impl AccessContext {
    pub(crate) fn root(db: &Database) -> Self {
        Self(Arc::new(ContextState {
            db: Some(db.clone()),
            clock: policy_clock(db),
            selected: AccessProfile::ReadOnly,
            parent: None,
            child_ceiling: AccessProfile::Standard,
            plan: AtomicBool::new(false),
            revision: AtomicU64::new(0),
            environment: Arc::new(Mutex::new((String::new(), 0))),
            #[cfg(test)]
            environment_override: Arc::new(Mutex::new(None)),
        }))
    }
    pub(crate) fn fixed(profile: AccessProfile) -> Self {
        Self(Arc::new(ContextState {
            db: None,
            clock: Arc::new(RwLock::new(0)),
            selected: profile,
            parent: None,
            child_ceiling: AccessProfile::Standard,
            plan: AtomicBool::new(false),
            revision: AtomicU64::new(0),
            environment: Arc::new(Mutex::new((String::new(), 0))),
            #[cfg(test)]
            environment_override: Arc::new(Mutex::new(None)),
        }))
    }
    /// Only server code can derive children; no parent/profile assertions in IPC.
    pub(crate) fn child(&self, selected: AccessProfile) -> Self {
        Self(Arc::new(ContextState {
            db: self.0.db.clone(),
            clock: self.0.clock.clone(),
            selected,
            parent: Some(self.clone()),
            child_ceiling: selected.max(self.profile()),
            plan: AtomicBool::new(false),
            revision: AtomicU64::new(0),
            environment: self.0.environment.clone(),
            #[cfg(test)]
            environment_override: self.0.environment_override.clone(),
        }))
    }
    pub(crate) fn set_plan(&self, plan: bool) {
        let _guard = self
            .0
            .clock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.0.plan.swap(plan, Ordering::SeqCst) != plan {
            self.0.revision.fetch_add(1, Ordering::SeqCst);
        }
    }
    pub(crate) fn invalidate(&self) {
        let _guard = self
            .0
            .clock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.0.revision.fetch_add(1, Ordering::SeqCst);
    }
    pub(crate) fn profile(&self) -> AccessProfile {
        if self.0.plan.load(Ordering::SeqCst) {
            return AccessProfile::ReadOnly;
        }
        let selected =
            self.0.db.as_ref().map_or(self.0.selected, |db| {
                match SettingsService::new(db).read(PROFILE_KEY) {
                    Ok(value) => AccessProfile::resolve(value.as_deref()),
                    Err(_) => AccessProfile::ReadOnly,
                }
            });
        selected.max(self.0.child_ceiling).max(
            self.0
                .parent
                .as_ref()
                .map_or(AccessProfile::Standard, Self::profile),
        )
    }
    pub(crate) fn execution_store(&self) -> Option<PermissionStore> {
        self.0.db.as_ref().map(PermissionStore::load)
    }
    pub(crate) fn change_policy(&self, action: impl FnOnce()) {
        let _guard = self
            .0
            .clock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.0.revision.fetch_add(1, Ordering::SeqCst);
        action();
    }
    pub(crate) fn store(&self) -> Option<PermissionStore> {
        self.0.db.as_ref().map(|db| {
            let store = PermissionStore::load(db);
            if store.is_unavailable() {
                self.invalidate();
            }
            store
        })
    }
    fn environment_identity(&self) -> String {
        let fingerprint = environment_fingerprint();
        #[cfg(test)]
        let fingerprint = self
            .0
            .environment_override
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or(fingerprint);
        let mut observed = self
            .0
            .environment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if observed.0 != fingerprint {
            observed.0 = fingerprint;
            observed.1 = observed.1.saturating_add(1);
        }
        format!("{}:{}", observed.0, observed.1)
    }
    #[cfg(test)]
    fn set_test_environment(&self, value: Option<&str>) {
        let _guard = self
            .0
            .clock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *self
            .0
            .environment_override
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            value.map(|value| format!("{:x}", Sha256::digest(value.as_bytes())));
    }
    fn revision(&self, epoch: u64) -> String {
        format!(
            "{epoch}:{}:{}",
            self.0.revision.load(Ordering::SeqCst),
            self.0
                .parent
                .as_ref()
                .map_or_else(String::new, |parent| parent.revision(epoch))
        )
    }
    pub(crate) fn allows(&self, call: &ToolCall) -> bool {
        self.profile() != AccessProfile::ReadOnly
            || matches!(
                call.name.as_str(),
                "read_file" | "list_directory" | "search_files"
            )
    }
    /// Includes exact arguments as well as canonical target: changing content,
    /// cwd, or command text cannot consume a consent for an earlier request.
    pub(crate) fn identity(
        &self,
        workspace: &Path,
        call: &ToolCall,
    ) -> Result<String, super::tools::ToolError> {
        self.with_identity(workspace, call, |identity, _, _| Ok(identity.to_owned()))
    }
    pub(crate) fn with_identity<T>(
        &self,
        workspace: &Path,
        call: &ToolCall,
        action: impl FnOnce(&str, &Path, &mut dyn FnMut()) -> Result<T, super::tools::ToolError>,
    ) -> Result<T, super::tools::ToolError> {
        let epoch = self
            .0
            .clock
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let args: serde_json::Value = serde_json::from_str(&call.arguments)
            .map_err(|_| super::tools::ToolError::InvalidArguments("invalid JSON".into()))?;
        let target = tool_scope(workspace, &call.name, &args)?;
        let mut hash = Sha256::new();
        for value in [
            &path_to_comparable_string(&canonical_root(workspace)),
            &call.name,
            &path_to_comparable_string(&target),
            &call.arguments,
            &self.environment_identity(),
            "PerCommandConsent:no-checkpoints",
            &format!("{:?}", self.profile()),
            &self.revision(*epoch),
        ] {
            hash.update(value.len().to_le_bytes());
            hash.update(value.as_bytes());
        }
        let mut epoch = Some(epoch);
        // The reader serializes validation through the actual handle-open/spawn.
        // The tool releases it once started; IO, traversal and process waits must
        // never prevent a live policy update or Stop. Errors drop it here too.
        action(&format!("{:x}", hash.finalize()), &target, &mut || {
            drop(epoch.take());
        })
    }
}
/// Values are hashed in process only. Neither environment names nor raw values
/// enter approval metadata, storage, logs or errors.
pub(crate) fn environment_fingerprint() -> String {
    let mut env: Vec<_> = std::env::vars_os().collect();
    env.sort();
    let mut hash = Sha256::new();
    for (key, value) in env {
        for item in [key, value] {
            let bytes = item.as_encoded_bytes();
            hash.update(bytes.len().to_le_bytes());
            hash.update(bytes);
        }
    }
    format!("{:x}", hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::approval::{ApprovalDecision, ApprovalGate, AutonomyMode};
    use crate::application::agent::control::CancellationToken;
    use crate::application::agent::tools::{
        test_support::{call, temp_workspace},
        ToolRegistry,
    };
    use crate::infrastructure::database::in_memory_database;
    use serde_json::json;
    fn approve(gate: &ApprovalGate, call: &ToolCall, group: bool) {
        let key = gate.group_key(call, "");
        let request_id = gate.prepare_pending_with_group(call, Some(key));
        assert!(gate.respond_with_scope(
            &request_id,
            ApprovalDecision::Approved,
            group.then_some("group")
        ));
        assert_eq!(
            gate.park_until_resolved(call),
            Ok(ApprovalDecision::Approved)
        );
    }
    #[test]
    fn missing_profile_is_standard_invalid_and_read_errors_fail_closed() {
        let db = in_memory_database();
        let access = AccessContext::root(&db);
        assert_eq!(access.profile(), AccessProfile::Standard);
        SettingsService::new(&db)
            .write(PROFILE_KEY, Some("unsupported"))
            .unwrap();
        assert_eq!(access.profile(), AccessProfile::ReadOnly);
        db.lock()
            .unwrap()
            .execute("DROP TABLE app_settings", [])
            .unwrap();
        assert_eq!(access.profile(), AccessProfile::ReadOnly);
    }
    #[test]
    fn root_plan_and_child_selected_ceilings_remain_live() {
        let db = in_memory_database();
        let root = AccessContext::root(&db);
        let child = root.child(AccessProfile::Supervised);
        assert_eq!(child.profile(), AccessProfile::Supervised);
        SettingsService::new(&db)
            .write(PROFILE_KEY, Some("read_only"))
            .unwrap();
        assert_eq!(root.profile(), AccessProfile::ReadOnly);
        assert_eq!(child.profile(), AccessProfile::ReadOnly);
        SettingsService::new(&db)
            .write(PROFILE_KEY, Some("standard"))
            .unwrap();
        assert_eq!(child.profile(), AccessProfile::Supervised);
        root.set_plan(true);
        assert_eq!(child.profile(), AccessProfile::ReadOnly);
        let plan_child = root.child(AccessProfile::Standard);
        root.set_plan(false);
        assert_eq!(plan_child.profile(), AccessProfile::ReadOnly);
    }
    #[test]
    fn each_profile_blocks_legacy_auto_and_shell_group_bypasses() {
        let workspace = temp_workspace();
        for profile in [
            AccessProfile::ReadOnly,
            AccessProfile::Standard,
            AccessProfile::Supervised,
        ] {
            let gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
                .with_access(AccessContext::fixed(profile));
            gate.bind_workspace(&workspace);
            for request in [
                call("write_file", json!({"path":"file.txt","content":"x"})),
                call("execute_command", json!({"command":"echo x","cwd":"."})),
            ] {
                assert!(gate.needs_approval(&request));
                assert!(ToolRegistry::execute_agent(
                    &request,
                    &workspace,
                    &CancellationToken::new(),
                    Some(&gate)
                )
                .is_err());
                let request_id =
                    gate.prepare_pending_with_group(&request, Some(gate.group_key(&request, "")));
                if request.name == "execute_command" || profile != AccessProfile::Standard {
                    assert!(!gate.respond_with_scope(
                        &request_id,
                        ApprovalDecision::Approved,
                        Some("group")
                    ));
                }
            }
        }
        assert!(!workspace.join("file.txt").exists());
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn exact_file_grant_cannot_cover_different_content_target_or_tool() {
        let workspace = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        gate.bind_workspace(&workspace);
        let request = call("write_file", json!({"path":"file.txt","content":"x"}));
        approve(&gate, &request, true);
        let execute = |request: &ToolCall| {
            ToolRegistry::execute_agent(request, &workspace, &CancellationToken::new(), Some(&gate))
        };
        execute(&request).unwrap();
        execute(&request).unwrap();
        for request in [
            call("write_file", json!({"path":"other.txt","content":"x"})),
            call("write_file", json!({"path":"file.txt","content":"changed"})),
            call(
                "edit_file",
                json!({"path":"file.txt","old_text":"x","new_text":"y"}),
            ),
        ] {
            assert!(execute(&request).is_err());
        }
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn policy_profile_clear_and_reversion_never_resurrect_consent() {
        use crate::application::agent::permissions::{delete_rule, insert_rule, RuleEffect};
        use crate::application::confirmations::{
            ConfirmationRegistry, ConfirmationTarget, OP_CLEAR_ALL_DATA, SCOPE_DATA_MANAGEMENT,
        };
        use crate::application::data_management::DataManagementService;
        let db = in_memory_database();
        let workspace = temp_workspace();
        let access = AccessContext::root(&db);
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous).with_access(access.clone());
        gate.bind_workspace(&workspace);
        let request = call("write_file", json!({"path":"file.txt","content":"x"}));
        for action in 0..8 {
            SettingsService::new(&db)
                .write(PROFILE_KEY, Some("standard"))
                .unwrap();
            approve(&gate, &request, true);
            let before = access.identity(&workspace, &request).unwrap();
            match action {
                0 => {
                    SettingsService::new(&db)
                        .write(PROFILE_KEY, Some("read_only"))
                        .unwrap();
                    SettingsService::new(&db).delete(PROFILE_KEY).unwrap();
                }
                1 => {
                    let id = insert_rule(&db, "coding", "write_file", None, RuleEffect::Deny, 0)
                        .unwrap();
                    delete_rule(&db, id).unwrap();
                }
                2 => {
                    SettingsService::new(&db)
                        .write("agent.autonomy", Some("supervised"))
                        .unwrap();
                    SettingsService::new(&db).delete("agent.autonomy").unwrap();
                }
                3 => {
                    let confirmations = ConfirmationRegistry::new();
                    let id = confirmations
                        .request(
                            SCOPE_DATA_MANAGEMENT,
                            &ConfirmationTarget::new(OP_CLEAR_ALL_DATA),
                        )
                        .unwrap();
                    DataManagementService::new(&db)
                        .clear(&id, &confirmations)
                        .unwrap();
                }
                4 => {
                    access.set_test_environment(Some("synthetic-env"));
                    assert_ne!(before, access.identity(&workspace, &request).unwrap());
                    access.set_test_environment(None);
                }
                6 => {
                    db.lock().unwrap().execute_batch("CREATE TRIGGER fail_profile BEFORE UPDATE ON app_settings BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
                    assert!(SettingsService::new(&db)
                        .write(PROFILE_KEY, Some("supervised"))
                        .is_err());
                    db.lock()
                        .unwrap()
                        .execute_batch("DROP TRIGGER fail_profile")
                        .unwrap();
                }
                7 => {
                    db.lock().unwrap().execute_batch("CREATE TRIGGER fail_clear BEFORE DELETE ON app_settings BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
                    let confirmations = ConfirmationRegistry::new();
                    let id = confirmations
                        .request(
                            SCOPE_DATA_MANAGEMENT,
                            &ConfirmationTarget::new(OP_CLEAR_ALL_DATA),
                        )
                        .unwrap();
                    assert!(DataManagementService::new(&db)
                        .clear(&id, &confirmations)
                        .is_err());
                    db.lock()
                        .unwrap()
                        .execute_batch("DROP TRIGGER fail_clear")
                        .unwrap();
                }
                _ => {
                    gate.set_mode(AutonomyMode::Supervised);
                    gate.set_mode(AutonomyMode::FullAutonomous);
                }
            }
            assert_ne!(before, access.identity(&workspace, &request).unwrap());
            assert!(ToolRegistry::execute_agent(
                &request,
                &workspace,
                &CancellationToken::new(),
                Some(&gate)
            )
            .is_err());
        }
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn pending_and_already_resolved_consent_are_revalidated_before_execution() {
        let db = in_memory_database();
        let workspace = temp_workspace();
        let access = AccessContext::root(&db);
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous).with_access(access.clone());
        gate.bind_workspace(&workspace);
        let request = call("write_file", json!({"path":"file.txt","content":"x"}));
        let request_id = gate.prepare_pending(&request);
        SettingsService::new(&db)
            .write(PROFILE_KEY, Some("supervised"))
            .unwrap();
        assert!(!gate.respond(&request_id, ApprovalDecision::Approved));
        assert_eq!(
            gate.park_until_resolved(&request),
            Ok(ApprovalDecision::Invalidated)
        );
        approve(&gate, &request, false);
        access.set_plan(true);
        assert!(ToolRegistry::execute_agent(
            &request,
            &workspace,
            &CancellationToken::new(),
            Some(&gate)
        )
        .is_err());
        assert!(!workspace.join("file.txt").exists());
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn sensitive_write_metadata_and_group_response_fail_closed() {
        let workspace = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        gate.bind_workspace(&workspace);
        for target in [
            ".env",
            ".env.local",
            "key.pem",
            "key.key",
            "id_rsa_backup",
            ".git/config",
            ".git/hooks/pre-commit",
            ".nexora/policy",
        ] {
            let request = call("write_file", json!({"path":target,"content":"x"}));
            let metadata = gate.metadata(&request);
            assert!(metadata.sensitive_write, "{target}");
            assert!(!metadata.allow_for_run);
            let request_id =
                gate.prepare_pending_with_group(&request, Some(gate.group_key(&request, "")));
            assert!(!gate.respond_with_scope(
                &request_id,
                ApprovalDecision::Approved,
                Some("group")
            ));
        }
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn workspace_reversion_and_client_capability_claims_do_not_restore_grants() {
        let first = temp_workspace();
        let second = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        gate.bind_workspace(&first);
        let request = call(
            "write_file",
            json!({"path":"file.txt","content":"x","checkpoints_available":true,"sandbox_posture":"Verified","parent_profile":"standard"}),
        );
        assert!(gate.needs_approval(&request));
        assert!(ToolRegistry::execute_agent(
            &request,
            &first,
            &CancellationToken::new(),
            Some(&gate)
        )
        .is_err());
        approve(&gate, &request, true);
        let request_id = gate.prepare_pending(&request);
        gate.bind_workspace(&second);
        gate.bind_workspace(&first);
        assert!(!gate.respond(&request_id, ApprovalDecision::Approved));
        assert_eq!(
            gate.park_until_resolved(&request),
            Ok(ApprovalDecision::Invalidated)
        );
        assert!(ToolRegistry::execute_agent(
            &request,
            &first,
            &CancellationToken::new(),
            Some(&gate)
        )
        .is_err());
        assert_eq!(gate.metadata(&request).sandbox_posture, "PerCommandConsent");
        assert!(!gate.metadata(&request).checkpoints_available);
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }
    #[test]
    fn unreadable_current_rules_deny_and_revoke_previously_reusable_consent() {
        let db = in_memory_database();
        let workspace = temp_workspace();
        let context = AccessContext::root(&db);
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous).with_access(context.clone());
        gate.bind_workspace(&workspace);
        let request = call("write_file", json!({"path":"file.txt","content":"x"}));
        approve(&gate, &request, true);
        let before = context.identity(&workspace, &request).unwrap();
        db.lock()
            .unwrap()
            .execute(
                "ALTER TABLE permission_rules RENAME TO unavailable_rules",
                [],
            )
            .unwrap();
        let store = context.store().unwrap();
        assert!(store.is_unavailable());
        assert!(matches!(
            store.decide(
                "coding",
                "read_file",
                None,
                crate::application::agent::approval::RiskClass::ReadOnly
            ),
            Some(
                crate::application::agent::permissions::PermissionOutcome::Deny {
                    rule_id: None,
                    ..
                }
            )
        ));
        db.lock()
            .unwrap()
            .execute(
                "ALTER TABLE unavailable_rules RENAME TO permission_rules",
                [],
            )
            .unwrap();
        assert_ne!(before, context.identity(&workspace, &request).unwrap());
        assert!(ToolRegistry::execute_agent(
            &request,
            &workspace,
            &CancellationToken::new(),
            Some(&gate)
        )
        .is_err());
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn direct_execution_rechecks_deny_and_ask_even_with_a_fresh_user_grant() {
        use crate::application::agent::permissions::{delete_rule, insert_rule, RuleEffect};
        let db = in_memory_database();
        let workspace = temp_workspace();
        std::fs::write(workspace.join("file.txt"), "synthetic").unwrap();
        let gate =
            ApprovalGate::new(AutonomyMode::FullAutonomous).with_access(AccessContext::root(&db));
        gate.bind_workspace(&workspace);
        let request = call("read_file", json!({"path":"file.txt"}));
        let deny = insert_rule(&db, "coding", "read_file", None, RuleEffect::Deny, 0).unwrap();
        approve(&gate, &request, false);
        assert!(ToolRegistry::execute_agent(
            &request,
            &workspace,
            &CancellationToken::new(),
            Some(&gate)
        )
        .is_err());
        delete_rule(&db, deny).unwrap();
        insert_rule(&db, "coding", "read_file", None, RuleEffect::Ask, 0).unwrap();
        assert!(!gate.metadata(&request).allow_for_run);
        assert!(ToolRegistry::execute_agent(
            &request,
            &workspace,
            &CancellationToken::new(),
            Some(&gate)
        )
        .is_err());
        approve(&gate, &request, false);
        assert_eq!(
            ToolRegistry::execute_agent(
                &request,
                &workspace,
                &CancellationToken::new(),
                Some(&gate)
            )
            .unwrap(),
            "synthetic"
        );
        assert!(ToolRegistry::execute_agent(
            &request,
            &workspace,
            &CancellationToken::new(),
            Some(&gate)
        )
        .is_err());
        std::fs::remove_dir_all(workspace).unwrap();
    }
    fn link_directory(target: &Path, alias: &Path) {
        #[cfg(windows)]
        {
            let output = std::process::Command::new("cmd")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(alias)
                .arg(target)
                .output()
                .unwrap();
            assert!(output.status.success(), "synthetic junction creation");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, alias).unwrap();
    }
    // Remove only the synthetic alias, never recurse into its target.
    fn unlink_directory(alias: &Path) {
        #[cfg(windows)]
        std::fs::remove_dir(alias).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(alias).unwrap();
    }
    #[test]
    fn search_directory_alias_retarget_invalidates_pending_and_reusable_consent() {
        let workspace = temp_workspace();
        let a = workspace.join("a");
        let b = workspace.join("b");
        let alias = workspace.join("alias");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(a.join("file.txt"), "needle A").unwrap();
        std::fs::write(b.join("file.txt"), "needle B").unwrap();
        link_directory(&a, &alias);
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        gate.bind_workspace(&workspace);
        let request = call(
            "search_files",
            json!({"pattern":"needle","path":".","directory":"alias"}),
        );
        let handle = gate.prepare_pending(&request);
        unlink_directory(&alias);
        link_directory(&b, &alias);
        assert!(!gate.respond(&handle, ApprovalDecision::Approved));
        assert_eq!(
            gate.park_until_resolved(&request),
            Ok(ApprovalDecision::Invalidated)
        );
        unlink_directory(&alias);
        link_directory(&a, &alias);
        approve(&gate, &request, true);
        let execute = || {
            ToolRegistry::execute_agent(
                &request,
                &workspace,
                &CancellationToken::new(),
                Some(&gate),
            )
        };
        assert!(execute().unwrap().contains("needle A"));
        assert!(
            execute().unwrap().contains("needle A"),
            "same canonical scope reuses exact consent"
        );
        unlink_directory(&alias);
        link_directory(&b, &alias);
        assert!(!gate.has_current_consent(&request));
        assert_eq!(
            execute(),
            Err(super::super::tools::ToolError::ConsentInvalidated)
        );
        approve(&gate, &request, false);
        let result = execute().unwrap();
        assert!(result.contains("needle B"));
        assert!(!result.contains("needle A"));
        unlink_directory(&alias);
        std::fs::remove_dir_all(&workspace).unwrap();
    }
    #[test]
    fn changed_alias_between_identity_and_open_is_rejected_before_observation() {
        let workspace = temp_workspace();
        let a = workspace.join("a");
        let b = workspace.join("b");
        let alias = workspace.join("alias");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        link_directory(&a, &alias);
        let request = call(
            "search_files",
            json!({"pattern":"needle","path":".","directory":"alias"}),
        );
        let access = AccessContext::fixed(AccessProfile::Standard);
        access
            .with_identity(&workspace, &request, |_, bound, _| {
                unlink_directory(&alias);
                link_directory(&b, &alias);
                assert_eq!(
                    super::super::tools::path_safety::execution_target(
                        &workspace,
                        "alias",
                        Some(bound)
                    ),
                    Err(super::super::tools::ToolError::ConsentInvalidated)
                );
                Ok(())
            })
            .unwrap();
        unlink_directory(&alias);
        std::fs::remove_dir_all(&workspace).unwrap();
    }
}
