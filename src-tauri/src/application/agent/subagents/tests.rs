use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::application::agent::access::{AccessContext, AccessProfile, PROFILE_KEY};
use crate::application::agent::approval::{ApprovalGate, AutonomyMode};
use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
use crate::application::agent::permissions::{
    self, is_subagent_tool, ApprovalResolveError, GroupScopeError, PermissionRule, PermissionStore,
    RuleEffect, RunPreset,
};
use crate::application::agent::registry::{ActiveAgentRun, AgentRunRegistry};
use crate::application::agent::runner::AgentRunner;
use crate::application::agent::tools::ToolRegistry;
use crate::application::execution::{
    AiRequest, AiResponse, ExecutorError, ProviderExecutor, TokenUsage, ToolCall,
};
use crate::application::flags::RunFlags;
use crate::application::settings::SettingsService;
use crate::infrastructure::database::in_memory_database;

use super::worktree::{PatchTransferOutcome, WorktreeIsolation};
use super::*;

fn temp_dir(label: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "nexora-subagent-test-{label}-{}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

fn init_git_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let out = cmd.output().expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    run(&["init"]);
    run(&["config", "user.email", "test@nexora.local"]);
    run(&["config", "user.name", "Nexora Test"]);
    run(&["config", "core.autocrlf", "false"]);
    fs::write(dir.join("shared.txt"), "line1\nline2\nline3\n").expect("write seed file");
    run(&["add", "-A"]);
    run(&["commit", "-m", "initial"]);
}

fn git_branch_list(dir: &Path) -> Vec<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(["branch", "--list", "nexora/subagent-*"])
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().expect("git branch");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().trim_start_matches(['*', '+']).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

type ExecHandler =
    Arc<dyn Fn(&AiRequest, &CancellationToken) -> Result<AiResponse, ExecutorError> + Send + Sync>;

/// Scriptable thread-safe executor for testing parallel subagent runs.
struct FnExecutor {
    handler: ExecHandler,
    recorded: Arc<Mutex<Vec<AiRequest>>>,
}

impl FnExecutor {
    fn new<F>(f: F) -> (Arc<Self>, Arc<Mutex<Vec<AiRequest>>>)
    where
        F: Fn(&AiRequest, &CancellationToken) -> Result<AiResponse, ExecutorError>
            + Send
            + Sync
            + 'static,
    {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let exec = Arc::new(Self {
            handler: Arc::new(f),
            recorded: Arc::clone(&recorded),
        });
        (exec, recorded)
    }
}

impl ProviderExecutor for FnExecutor {
    fn execute(
        &self,
        request: &AiRequest,
        _credential: &str,
        cancel: &CancellationToken,
    ) -> Result<AiResponse, ExecutorError> {
        self.recorded.lock().unwrap().push(request.clone());
        (self.handler)(request, cancel)
    }
}

fn text_reply(text: &str) -> AiResponse {
    AiResponse {
        content: text.to_string(),
        model: "gpt-5.6-terra".to_string(),
        usage: Some(TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
        }),
        tool_calls: Vec::new(),
    }
}

fn tool_reply(calls: Vec<ToolCall>) -> AiResponse {
    AiResponse {
        content: String::new(),
        model: "gpt-5.6-terra".to_string(),
        usage: Some(TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
        }),
        tool_calls: calls,
    }
}

fn make_call(id: &str, name: &str, args: &serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: args.to_string(),
        thought_signature: None,
    }
}

fn make_manager(
    ws: &Path,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    parent_control: RunControl,
    parent_gate: ApprovalGate,
    max_concurrency: usize,
) -> (Arc<SubagentManager>, mpsc::Receiver<AgentRunEvent>) {
    let (tx, rx) = mpsc::channel();
    let mgr = SubagentManager::new(SubagentManagerConfig {
        parent_run_id: 42,
        db: None,
        executor,
        workspace_root: ws.to_path_buf(),
        provider: "openai".to_string(),
        model: "gpt-5.4".to_string(),
        effort: Some("medium".to_string()),
        credential: "test-secret-credential".to_string(),
        parent_control,
        parent_gate,
        parent_preset: RunPreset::Coding,
        parent_is_plan: false,
        permission_store: None,
        project_instructions: None,
        run_flags: RunFlags::default(),
        event_sender: Some(tx),
        max_concurrency,
    });
    (mgr, rx)
}

#[test]
fn clamp_and_enforce_concurrency_bounds_1_to_6() {
    assert_eq!(DEFAULT_SUBAGENT_MAX_CONCURRENCY, 3);
    assert_eq!(clamp_max_concurrency(0), 1);
    assert_eq!(clamp_max_concurrency(3), 3);
    assert_eq!(clamp_max_concurrency(6), 6);
    assert_eq!(clamp_max_concurrency(99), 6);

    let ws = temp_dir("concurrency");
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let release_clone = Arc::clone(&release);
    let (exec, _rec) = FnExecutor::new(move |_req, cancel| {
        let (lock, cvar) = &*release_clone;
        let mut go = lock.lock().unwrap();
        while !*go && !cancel.is_cancelled() {
            let (next, _) = cvar.wait_timeout(go, Duration::from_millis(20)).unwrap();
            go = next;
        }
        if cancel.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        Ok(text_reply("finished"))
    });

    let control = RunControl::new();
    let gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
        .with_access(AccessContext::fixed(AccessProfile::Standard));
    let (mgr, _rx) = make_manager(&ws, exec, control, gate, 2);
    assert_eq!(mgr.max_concurrency(), 2);

    let r1 = mgr
        .dispatch_tool("spawn_subagent", r#"{"task":"first","wait":false}"#)
        .unwrap();
    assert!(r1.contains("sub-1"), "got {r1}");
    let r2 = mgr
        .dispatch_tool("spawn_subagent", r#"{"task":"second","wait":false}"#)
        .unwrap();
    assert!(r2.contains("sub-2"), "got {r2}");
    let r3 = mgr
        .dispatch_tool("spawn_subagent", r#"{"task":"third","wait":false}"#)
        .unwrap();
    assert!(
        r3.contains("maximum concurrent subagents (2) reached"),
        "expected concurrency rejection, got {r3}"
    );

    // Release and wait.
    {
        let (lock, cvar) = &*release;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }
    let waited = mgr.dispatch_tool("wait_subagent", "{}").unwrap();
    assert!(waited.contains("sub-1") && waited.contains("sub-2"));
    mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn max_depth_1_omits_subagent_tools_and_fails_closed_in_dispatch() {
    let ws = temp_dir("depth1");
    let calls_seen = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls_seen);
    let (exec, recorded) = FnExecutor::new(move |req, _cancel| {
        let turn = calls_clone.fetch_add(1, Ordering::SeqCst);
        if turn == 0 {
            // Child attempts to smuggle `spawn_subagent` despite omission from schema.
            assert!(
                req.tools.iter().all(|t| !is_subagent_tool(&t.name)),
                "subagent tool schema MUST omit all subagent tools at depth=1"
            );
            Ok(tool_reply(vec![make_call(
                "smuggled-1",
                "spawn_subagent",
                &serde_json::json!({ "task": "nested illegal child" }),
            )]))
        } else {
            Ok(text_reply("child handled denial"))
        }
    });

    let control = RunControl::new();
    let gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
        .with_access(AccessContext::fixed(AccessProfile::Standard));
    let (mgr, _rx) = make_manager(&ws, exec, control, gate, 3);

    let out = mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"try nested spawn","wait":true}"#,
        )
        .unwrap();
    assert!(out.contains("completed"), "got {out}");

    let reqs = recorded.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    let denial_obs = reqs[1]
        .messages
        .iter()
        .find_map(|m| m.tool_result.as_ref())
        .map_or("", |r| r.content.as_str());
    assert!(
        denial_obs.contains("maximum subagent nesting depth of 1 reached"),
        "expected fail-closed depth=1 denial, got: {denial_obs}"
    );
    // Only the single child exists — no nested subagent spawned.
    assert_eq!(mgr.snapshots().len(), 1);
    mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn model_defaults_to_parent_and_validates_m06_supported_efforts() {
    // 1. Default inherits parent model verbatim (NEVER auto-escalates to pricier model).
    let (m, e) = resolve_model_and_effort(
        None,
        "gemini",
        "gemini-3.1-flash-lite",
        Some("high"),
        None,
        None,
    )
    .expect("default succeeds");
    assert_eq!(m, "gemini-3.1-flash-lite");
    // Lite model does not support effort, so incompatible parent effort is dropped.
    assert_eq!(e, None);

    // 2. Explicit valid effort on a thinking model succeeds.
    let (m2, e2) = resolve_model_and_effort(
        None,
        "gemini",
        "gemini-3.6-flash",
        Some("low"),
        None,
        Some("high"),
    )
    .expect("high is supported on gemini-3.6-flash");
    assert_eq!(m2, "gemini-3.6-flash");
    assert_eq!(e2.as_deref(), Some("high"));

    // 3. Explicit unsupported effort on gemini-3.6-flash (`xhigh` not in supported_efforts) fails loudly.
    let err_xhigh = resolve_model_and_effort(
        None,
        "gemini",
        "gemini-3.6-flash",
        None,
        None,
        Some("xhigh"),
    )
    .expect_err("xhigh must be rejected for gemini-3.6-flash");
    assert!(err_xhigh.contains("not supported"), "got {err_xhigh}");

    // 4. Explicit effort on non-thinking lite model fails loudly.
    let err_lite = resolve_model_and_effort(
        None,
        "gemini",
        "gemini-3.1-flash-lite",
        None,
        None,
        Some("low"),
    )
    .expect_err("effort must be rejected for lite model");
    assert!(
        err_lite.contains("does not support reasoning effort"),
        "got {err_lite}"
    );

    // 5. Explicit model override + valid xhigh on openai gpt-5.6-terra succeeds.
    let (m3, e3) = resolve_model_and_effort(
        None,
        "openai",
        "gpt-5.6-luna",
        Some("low"),
        Some("gpt-5.6-terra"),
        Some("xhigh"),
    )
    .expect("xhigh supported on openai");
    assert_eq!(m3, "gpt-5.6-terra");
    assert_eq!(e3.as_deref(), Some("xhigh"));
}

#[test]
fn lifetime_child_access_ceiling_enforces_parent_downgrade_and_plan_mode() {
    let db = in_memory_database();
    let ws = temp_dir("ceiling");
    let root_access = AccessContext::root(&db);
    assert_eq!(root_access.profile(), AccessProfile::Standard);

    // Part A: parent downgrades mid-run before child attempts write_file.
    let db_downgrade = db.clone();
    let turn_counter = Arc::new(AtomicUsize::new(0));
    let turn_clone = Arc::clone(&turn_counter);
    let (exec, recorded) = FnExecutor::new(move |_req, _cancel| {
        let t = turn_clone.fetch_add(1, Ordering::SeqCst);
        if t == 0 {
            // Simulate user downgrading parent access to read_only while child is live!
            SettingsService::new(&db_downgrade)
                .write(PROFILE_KEY, Some("read_only"))
                .unwrap();
            Ok(tool_reply(vec![make_call(
                "w1",
                "write_file",
                &serde_json::json!({ "path": "forbidden.txt", "content": "hacked" }),
            )]))
        } else {
            Ok(text_reply("done after downgrade"))
        }
    });

    let control = RunControl::new();
    let gate = ApprovalGate::new(AutonomyMode::FullAutonomous).with_access(root_access.clone());
    let (mgr, _rx) = make_manager(&ws, exec, control, gate, 3);

    let spawn_out = mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"try writing","access":"workspace_write","wait":true}"#,
        )
        .unwrap();
    assert!(spawn_out.contains("completed"), "got {spawn_out}");
    assert!(
        !ws.join("forbidden.txt").exists(),
        "child write MUST be blocked after live parent downgrade to read_only"
    );
    let captured_reqs = recorded.lock().unwrap();
    let obs = captured_reqs[1]
        .messages
        .iter()
        .find_map(|m| m.tool_result.as_ref())
        .map_or("", |r| r.content.as_str());
    assert!(
        obs.contains("blocked by the effective access profile"),
        "expected access profile denial, got {obs}"
    );
    mgr.shutdown_and_join();

    // Part B: parent in Plan mode strictly caps child to read_only & disables worktree.
    SettingsService::new(&db)
        .write(PROFILE_KEY, Some("standard"))
        .unwrap();
    let (exec_plan, _) = FnExecutor::new(|_req, _cancel| Ok(text_reply("plan child done")));
    let plan_mgr = SubagentManager::new(SubagentManagerConfig {
        parent_run_id: 43,
        db: Some(db),
        executor: exec_plan,
        workspace_root: ws.clone(),
        provider: "openai".to_string(),
        model: "gpt-5.4".to_string(),
        effort: None,
        credential: "secret".to_string(),
        parent_control: RunControl::new(),
        parent_gate: ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard)),
        parent_preset: RunPreset::Coding,
        parent_is_plan: true,
        permission_store: None,
        project_instructions: None,
        run_flags: RunFlags::default(),
        event_sender: None,
        max_concurrency: 3,
    });
    let plan_out = plan_mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"research","access":"full_access","use_worktree":true,"wait":true}"#,
        )
        .unwrap();
    assert!(
        plan_out.contains("\"access\":\"read_only\""),
        "got {plan_out}"
    );
    assert!(
        plan_out.contains("\"worktree_isolated\":false"),
        "got {plan_out}"
    );
    plan_mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn send_subagent_message_continues_existing_conversation_context() {
    let ws = temp_dir("send-msg");
    let turn = Arc::new(AtomicUsize::new(0));
    let turn_clone = Arc::clone(&turn);
    let (exec, recorded) = FnExecutor::new(move |_req, _cancel| {
        let t = turn_clone.fetch_add(1, Ordering::SeqCst);
        if t == 0 {
            Ok(text_reply("initial findings: module A has 3 functions"))
        } else {
            Ok(text_reply("follow-up answer using prior context"))
        }
    });

    let control = RunControl::new();
    let gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
        .with_access(AccessContext::fixed(AccessProfile::Standard));
    let (mgr, _rx) = make_manager(&ws, exec, control, gate, 3);

    let first = mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"inspect module A","wait":true}"#,
        )
        .unwrap();
    assert!(first.contains("initial findings"), "got {first}");

    let second = mgr
        .dispatch_tool(
            "send_subagent_message",
            r#"{"subagent_id":"sub-1","message":"now check edge cases","wait":true}"#,
        )
        .unwrap();
    assert!(second.contains("follow-up answer"), "got {second}");

    let reqs = recorded.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    let second_msgs = &reqs[1].messages;
    assert!(
        second_msgs
            .iter()
            .any(|m| m.content.contains("inspect module A")),
        "follow-up must preserve initial user message"
    );
    assert!(
        second_msgs.iter().any(|m| m
            .content
            .contains("initial findings: module A has 3 functions")),
        "follow-up must preserve prior assistant reply"
    );
    assert!(
        second_msgs
            .iter()
            .any(|m| m.content.contains("now check edge cases")),
        "follow-up must include new user message"
    );
    mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn kill_subagent_stops_individual_child_without_cancelling_parent() {
    let ws = temp_dir("kill-one");
    let entered = Arc::new(AtomicBool::new(false));
    let entered_clone = Arc::clone(&entered);
    let (exec, _) = FnExecutor::new(move |_req, cancel| {
        entered_clone.store(true, Ordering::SeqCst);
        while !cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(ExecutorError::Cancelled)
    });

    let parent_control = RunControl::new();
    let gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
        .with_access(AccessContext::fixed(AccessProfile::Standard));
    let (mgr, _rx) = make_manager(&ws, exec, parent_control.clone(), gate, 3);

    let spawned = mgr
        .dispatch_tool("spawn_subagent", r#"{"task":"long job","wait":false}"#)
        .unwrap();
    assert!(spawned.contains("sub-1"));

    let deadline = Instant::now() + Duration::from_secs(2);
    while !entered.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(mgr.kill_subagent("sub-1"));
    assert!(
        !parent_control.is_cancelled(),
        "killing one subagent MUST NOT cancel the parent run"
    );

    let snaps = mgr.snapshots();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].status, "cancelled");

    // Killed subagent cannot be resumed.
    let resume = mgr
        .dispatch_tool(
            "send_subagent_message",
            r#"{"subagent_id":"sub-1","message":"wake up"}"#,
        )
        .unwrap();
    assert!(resume.contains("was killed"), "got {resume}");

    mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn max_turns_terminates_subagent_deterministically() {
    let ws = temp_dir("max-turns");
    let (exec, _) = FnExecutor::new(|_req, _cancel| {
        Ok(tool_reply(vec![make_call(
            "ls-1",
            "list_directory",
            &serde_json::json!({}),
        )]))
    });

    let control = RunControl::new();
    let gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
        .with_access(AccessContext::fixed(AccessProfile::Standard));
    let (mgr, _rx) = make_manager(&ws, exec, control, gate, 3);

    let out = mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"loop forever","max_turns":1,"wait":true}"#,
        )
        .unwrap();
    assert!(out.contains("\"status\":\"failed\""), "got {out}");
    assert!(out.contains("max_turns"), "got {out}");
    mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn timeout_cancels_runaway_subagent_without_cancelling_parent() {
    let ws = temp_dir("timeout");
    let (exec, _) = FnExecutor::new(|_req, cancel| {
        while !cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(ExecutorError::Cancelled)
    });

    let parent_control = RunControl::new();
    let gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
        .with_access(AccessContext::fixed(AccessProfile::Standard));
    let (mgr, _rx) = make_manager(&ws, exec, parent_control.clone(), gate, 3);

    let out = mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"hang forever","timeout_secs":5,"wait":true}"#,
        )
        .unwrap();
    assert!(out.contains("\"status\":\"timed_out\""), "got {out}");
    assert!(
        !parent_control.is_cancelled(),
        "subagent timeout must not cancel parent run"
    );
    mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
#[allow(clippy::too_many_lines)]
fn worktree_isolation_applies_clean_patch_and_cleans_up_on_completion() {
    let ws = temp_dir("wt-clean");
    init_git_repo(&ws);

    // Read-only subagent shares parent workspace even if use_worktree is requested.
    let (read_only_exec, _) = FnExecutor::new(|_req, _cancel| Ok(text_reply("read done")));
    let (read_only_mgr, _) = make_manager(
        &ws,
        read_only_exec,
        RunControl::new(),
        ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard)),
        3,
    );
    let read_only_out = read_only_mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"inspect","access":"read_only","use_worktree":true,"wait":true}"#,
        )
        .unwrap();
    assert!(
        read_only_out.contains("\"worktree_isolated\":false"),
        "got {read_only_out}"
    );
    assert!(git_branch_list(&ws).is_empty());
    read_only_mgr.shutdown_and_join();

    // Writing subagent edits tracked file + creates new file inside isolated worktree.
    let turn = Arc::new(AtomicUsize::new(0));
    let turn_clone = Arc::clone(&turn);
    let (writer_exec, _) = FnExecutor::new(move |_req, _cancel| {
        let t = turn_clone.fetch_add(1, Ordering::SeqCst);
        if t == 0 {
            Ok(tool_reply(vec![
                make_call(
                    "c1",
                    "write_file",
                    &serde_json::json!({
                        "path": "shared.txt",
                        "content": "line1\nupdated-by-subagent\nline3\n"
                    }),
                ),
                make_call(
                    "c2",
                    "write_file",
                    &serde_json::json!({
                        "path": "new_from_subagent.txt",
                        "content": "hello isolated world\n"
                    }),
                ),
            ]))
        } else {
            Ok(text_reply("edits finished"))
        }
    });
    let (writer_mgr, writer_rx) = make_manager(
        &ws,
        writer_exec,
        RunControl::new(),
        ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard)),
        3,
    );
    // Approve child write_file calls flowing through parent event channel.
    let mgr_for_approver = Arc::clone(&writer_mgr);
    let stop_approver = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop_approver);
    let approver = std::thread::spawn(move || {
        while !stop_flag.load(Ordering::SeqCst) {
            match writer_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(AgentRunEvent::ApprovalRequested { call_id, .. }) => {
                    assert!(mgr_for_approver.resolve_approval(
                        &call_id,
                        ApprovalDecision::Approved,
                        None
                    ));
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    let write_out = writer_mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"edit files","access":"workspace_write","use_worktree":true,"wait":true}"#,
        )
        .unwrap();
    stop_approver.store(true, Ordering::SeqCst);
    let _ = approver.join();
    assert!(
        write_out.contains("\"status\":\"completed\""),
        "got {write_out}"
    );
    assert!(
        write_out.contains("\"worktree_isolated\":true"),
        "got {write_out}"
    );
    assert!(
        write_out.contains("\"patch_applied\":true"),
        "got {write_out}"
    );
    assert!(
        write_out.contains("\"patch_conflict\":false"),
        "got {write_out}"
    );

    // Changes cleanly transferred to parent workspace.
    assert_eq!(
        fs::read_to_string(ws.join("shared.txt")).unwrap(),
        "line1\nupdated-by-subagent\nline3\n"
    );
    assert_eq!(
        fs::read_to_string(ws.join("new_from_subagent.txt")).unwrap(),
        "hello isolated world\n"
    );
    // Temporary branch cleaned up.
    assert!(
        git_branch_list(&ws).is_empty(),
        "temporary worktree branch must be deleted on completion"
    );

    writer_mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn worktree_conflict_fails_cleanly_without_corrupting_parent_and_cleans_up() {
    let ws = temp_dir("wt-conflict");
    init_git_repo(&ws);

    let wt = WorktreeIsolation::create(&ws, 99, "sub-conflict").expect("create worktree");
    let wt_root = wt.worktree_root().to_path_buf();
    let branch = wt.branch_name().to_string();
    assert!(wt_root.exists());
    assert!(git_branch_list(&ws).contains(&branch));

    // Modify same lines differently in both parent and worktree.
    fs::write(ws.join("shared.txt"), "parent-change\nline2\nline3\n").unwrap();
    fs::write(
        wt.workspace_path().join("shared.txt"),
        "subagent-change\nline2\nline3\n",
    )
    .unwrap();

    let lock = Mutex::new(());
    let outcome = wt.apply_to_parent(&lock);
    assert!(
        matches!(outcome, PatchTransferOutcome::Conflict { .. }),
        "expected clean conflict, got {outcome:?}"
    );
    // Parent file untouched, zero conflict markers.
    assert_eq!(
        fs::read_to_string(ws.join("shared.txt")).unwrap(),
        "parent-change\nline2\nline3\n"
    );

    // Drop triggers mandatory cleanup of worktree dir and temporary branch.
    drop(wt);
    assert!(!wt_root.exists(), "worktree dir must be removed on Drop");
    assert!(
        !git_branch_list(&ws).contains(&branch),
        "temporary branch must be deleted on Drop"
    );
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn parent_runner_exposes_subagent_tools_only_when_manager_attached() {
    let without = ToolRegistry::definitions_for_run_full(RunPreset::Coding, false, &[], false);
    assert!(without.iter().all(|d| !is_subagent_tool(&d.name)));

    let with = ToolRegistry::definitions_for_run_full(RunPreset::Coding, false, &[], true);
    for name in [
        "spawn_subagent",
        "send_subagent_message",
        "wait_subagent",
        "kill_subagent",
        "list_subagents",
    ] {
        assert!(
            with.iter().any(|d| d.name == name),
            "expected parent schema to include {name}"
        );
    }

    // End-to-end parent AgentRunner invoking spawn_subagent + list_subagents.
    let ws = temp_dir("runner-e2e");
    let parent_turn = Arc::new(AtomicUsize::new(0));
    let pt_clone = Arc::clone(&parent_turn);
    let (exec, _) = FnExecutor::new(move |req, _cancel| {
        // Distinguish child requests (no subagent tools) from parent requests.
        let is_parent = req.tools.iter().any(|t| t.name == "spawn_subagent");
        if !is_parent {
            return Ok(text_reply("child analysis ok"));
        }
        let t = pt_clone.fetch_add(1, Ordering::SeqCst);
        if t == 0 {
            Ok(tool_reply(vec![make_call(
                "p1",
                "spawn_subagent",
                &serde_json::json!({ "task": "analyze repo", "wait": true }),
            )]))
        } else {
            Ok(text_reply("parent synthesized child result"))
        }
    });

    let control = RunControl::new();
    let gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
        .with_access(AccessContext::fixed(AccessProfile::Standard));
    let (mgr, rx) = make_manager(
        &ws,
        Arc::clone(&exec) as _,
        control.clone(),
        gate.clone(),
        3,
    );

    let out = AgentRunner::new(exec.as_ref(), &ws)
        .with_control(control)
        .with_approval_gate(gate)
        .with_subagent_manager(Arc::clone(&mgr))
        .run("openai", "gpt-5.4", "secret", "delegate analysis")
        .unwrap();
    assert_eq!(out, "parent synthesized child result");

    let events: Vec<AgentRunEvent> = rx.try_iter().collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentRunEvent::SubagentUpdated { .. })),
        "expected SubagentUpdated events"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentRunEvent::SubagentStep { .. })),
        "expected SubagentStep events"
    );
    let _ = fs::remove_dir_all(&ws);
}

#[test]
#[allow(clippy::too_many_lines)]
fn coding_scoped_deny_rule_still_blocks_read_only_and_workspace_write_subagents() {
    let ws = temp_dir("coding-deny");
    fs::create_dir_all(ws.join("private")).unwrap();
    fs::create_dir_all(ws.join("protected")).unwrap();
    fs::write(ws.join("private/secret.txt"), "top-secret-value").unwrap();
    fs::write(ws.join("allowed.txt"), "ok").unwrap();

    let store = PermissionStore::from_rules(vec![
        PermissionRule {
            id: 101,
            preset: "coding".to_string(),
            tool_pattern: "read_file".to_string(),
            path_pattern: Some("private".to_string()),
            effect: RuleEffect::Deny,
            priority: 10,
        },
        PermissionRule {
            id: 102,
            preset: "coding".to_string(),
            tool_pattern: "write_file".to_string(),
            path_pattern: Some("protected".to_string()),
            effect: RuleEffect::Deny,
            priority: 10,
        },
    ]);

    // Part A: ReadOnly subagent under Coding parent keeps Coding preset rules
    // while hiding write/shell tools via TaskCapability + ToolRegistry.
    let ro_turn = Arc::new(AtomicUsize::new(0));
    let ro_turn_clone = Arc::clone(&ro_turn);
    let (ro_exec, ro_recorded) = FnExecutor::new(move |req, _cancel| {
        let t = ro_turn_clone.fetch_add(1, Ordering::SeqCst);
        assert!(
            req.tools.iter().all(|d| d.name != "execute_command"
                && d.name != "write_file"
                && d.name != "edit_file"),
            "ReadOnly subagent must hide shell and mutating tools from schema"
        );
        if t == 0 {
            Ok(tool_reply(vec![make_call(
                "r1",
                "read_file",
                &serde_json::json!({ "path": "private/secret.txt" }),
            )]))
        } else {
            Ok(text_reply("read-only done"))
        }
    });
    let ro_mgr = SubagentManager::new(SubagentManagerConfig {
        parent_run_id: 50,
        db: None,
        executor: ro_exec,
        workspace_root: ws.clone(),
        provider: "openai".to_string(),
        model: "gpt-5.4".to_string(),
        effort: None,
        credential: "secret".to_string(),
        parent_control: RunControl::new(),
        parent_gate: ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard)),
        parent_preset: RunPreset::Coding,
        parent_is_plan: false,
        permission_store: Some(store.clone()),
        project_instructions: None,
        run_flags: RunFlags::default(),
        event_sender: None,
        max_concurrency: 3,
    });
    let ro_out = ro_mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"read secret","access":"read_only","wait":true}"#,
        )
        .unwrap();
    assert!(ro_out.contains("\"status\":\"completed\""), "got {ro_out}");
    let ro_reqs = ro_recorded.lock().unwrap();
    let ro_obs = ro_reqs[1]
        .messages
        .iter()
        .find_map(|m| m.tool_result.as_ref())
        .map_or("", |r| r.content.as_str());
    assert!(
        ro_obs.contains("tool execution was denied"),
        "coding-scoped Deny rule must block ReadOnly subagent read_file; got: {ro_obs}"
    );
    assert!(
        !ro_obs.contains("top-secret-value"),
        "denied file contents must never leak"
    );
    ro_mgr.shutdown_and_join();

    // Part B: WorkspaceWrite subagent under Coding parent keeps Coding preset
    // rules, hides execute_command from schema, and denies smuggled shell calls.
    let ww_turn = Arc::new(AtomicUsize::new(0));
    let ww_turn_clone = Arc::clone(&ww_turn);
    let (ww_exec, ww_recorded) = FnExecutor::new(move |req, _cancel| {
        let t = ww_turn_clone.fetch_add(1, Ordering::SeqCst);
        assert!(
            req.tools.iter().any(|d| d.name == "write_file"),
            "WorkspaceWrite subagent should expose write_file"
        );
        assert!(
            req.tools.iter().all(|d| d.name != "execute_command"),
            "WorkspaceWrite subagent must hide execute_command from schema"
        );
        if t == 0 {
            Ok(tool_reply(vec![
                make_call(
                    "w1",
                    "write_file",
                    &serde_json::json!({ "path": "protected/out.txt", "content": "hacked" }),
                ),
                make_call(
                    "sh1",
                    "execute_command",
                    &serde_json::json!({ "command": "echo forbidden" }),
                ),
            ]))
        } else {
            Ok(text_reply("workspace-write done"))
        }
    });
    let ww_mgr = SubagentManager::new(SubagentManagerConfig {
        parent_run_id: 51,
        db: None,
        executor: ww_exec,
        workspace_root: ws.clone(),
        provider: "openai".to_string(),
        model: "gpt-5.4".to_string(),
        effort: None,
        credential: "secret".to_string(),
        parent_control: RunControl::new(),
        parent_gate: ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard)),
        parent_preset: RunPreset::Coding,
        parent_is_plan: false,
        permission_store: Some(store),
        project_instructions: None,
        run_flags: RunFlags::default(),
        event_sender: None,
        max_concurrency: 3,
    });
    let ww_out = ww_mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"write protected","access":"workspace_write","wait":true}"#,
        )
        .unwrap();
    assert!(ww_out.contains("\"status\":\"completed\""), "got {ww_out}");
    assert!(
        !ws.join("protected/out.txt").exists(),
        "coding-scoped Deny rule must prevent WorkspaceWrite subagent from writing file"
    );
    let ww_reqs = ww_recorded.lock().unwrap();
    let ww_obs: Vec<&str> = ww_reqs[1]
        .messages
        .iter()
        .filter_map(|m| m.tool_result.as_ref().map(|r| r.content.as_str()))
        .collect();
    assert!(
        ww_obs
            .iter()
            .any(|o| o.contains("tool execution was denied")),
        "expected coding-scoped Deny observation for write_file, got: {ww_obs:?}"
    );
    assert!(
        ww_obs
            .iter()
            .any(|o| o.contains("not available under task capability 'workspace_write'")),
        "expected capability denial for smuggled execute_command, got: {ww_obs:?}"
    );
    ww_mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
#[allow(clippy::too_many_lines)]
fn killed_worktree_subagent_skips_apply_to_parent_and_defers_cleanup_until_exit() {
    let ws = temp_dir("wt-kill-gate");
    init_git_repo(&ws);

    let in_turn_1 = Arc::new((Mutex::new(false), Condvar::new()));
    let release_turn_1 = Arc::new((Mutex::new(false), Condvar::new()));
    let in_turn_1_clone = Arc::clone(&in_turn_1);
    let release_turn_1_clone = Arc::clone(&release_turn_1);
    let turn = Arc::new(AtomicUsize::new(0));
    let turn_clone = Arc::clone(&turn);

    let (exec, _) = FnExecutor::new(move |_req, _cancel| {
        let t = turn_clone.fetch_add(1, Ordering::SeqCst);
        if t == 0 {
            Ok(tool_reply(vec![make_call(
                "w1",
                "write_file",
                &serde_json::json!({
                    "path": "shared.txt",
                    "content": "line1\nmodified-by-killed-subagent\nline3\n"
                }),
            )]))
        } else {
            {
                let (lock, cvar) = &*in_turn_1_clone;
                *lock.lock().unwrap() = true;
                cvar.notify_all();
            }
            {
                let (lock, cvar) = &*release_turn_1_clone;
                let mut go = lock.lock().unwrap();
                while !*go {
                    go = cvar.wait(go).unwrap();
                }
            }
            // Deliberately ignore cancel and return Ok to verify apply_to_parent
            // is gated on !killed && !child_control.is_cancelled().
            Ok(text_reply("sneaky completion after kill"))
        }
    });

    let (mgr, rx) = make_manager(
        &ws,
        exec,
        RunControl::new(),
        ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard)),
        3,
    );
    let mgr_for_approver = Arc::clone(&mgr);
    let stop_approver = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop_approver);
    let approver = std::thread::spawn(move || {
        while !stop_flag.load(Ordering::SeqCst) {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(AgentRunEvent::ApprovalRequested { call_id, .. }) => {
                    let _ = mgr_for_approver.resolve_approval(
                        &call_id,
                        ApprovalDecision::Approved,
                        None,
                    );
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    let spawned = mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"edit then stall","access":"workspace_write","use_worktree":true,"wait":false}"#,
        )
        .unwrap();
    assert!(spawned.contains("sub-1"), "got {spawned}");

    // Wait until turn 0's write_file completed and turn 1 is mid-flight.
    {
        let (lock, cvar) = &*in_turn_1;
        let mut entered = lock.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !*entered {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for child turn 1");
            let (next, _) = cvar.wait_timeout(entered, remaining).unwrap();
            entered = next;
        }
    }

    assert!(
        !git_branch_list(&ws).is_empty(),
        "worktree branch should exist while child is running"
    );
    assert!(mgr.kill_subagent("sub-1"));

    // Worktree MUST NOT be cleaned up while the child thread is still mid-flight.
    assert!(
        !git_branch_list(&ws).is_empty(),
        "kill_subagent must not race wt.cleanup() while the child thread is still active"
    );

    // Release child thread and wait for clean exit + deferred worktree cleanup.
    {
        let (lock, cvar) = &*release_turn_1;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }
    stop_approver.store(true, Ordering::SeqCst);
    let _ = approver.join();
    mgr.shutdown_and_join();

    assert_eq!(
        fs::read_to_string(ws.join("shared.txt")).unwrap(),
        "line1\nline2\nline3\n",
        "killed subagent worktree patch MUST NOT be applied to parent workspace"
    );
    let snaps = mgr.snapshots();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].status, "cancelled");
    assert!(!snaps[0].patch_applied);
    assert!(
        git_branch_list(&ws).is_empty(),
        "worktree branch must be cleaned up once the child thread exits"
    );
    let _ = fs::remove_dir_all(&ws);
}

#[test]
#[allow(clippy::too_many_lines)]
fn pending_scope_info_and_group_confirm_resolve_child_subagent_approval() {
    let db = in_memory_database();
    let ws = temp_dir("subagent-group-confirm");

    let turn = Arc::new(AtomicUsize::new(0));
    let turn_clone = Arc::clone(&turn);
    let (exec, _) = FnExecutor::new(move |_req, _cancel| {
        let t = turn_clone.fetch_add(1, Ordering::SeqCst);
        if t == 0 {
            Ok(tool_reply(vec![make_call(
                "cw1",
                "write_file",
                &serde_json::json!({ "path": "docs/note.md", "content": "hello child" }),
            )]))
        } else {
            Ok(text_reply("child wrote docs/note.md"))
        }
    });

    let parent_control = RunControl::new();
    let parent_gate = ApprovalGate::new(AutonomyMode::Supervised)
        .with_access(AccessContext::fixed(AccessProfile::Standard));
    let (mgr, rx) = make_manager(&ws, exec, parent_control.clone(), parent_gate.clone(), 3);

    let registry = AgentRunRegistry::default();
    registry.register(
        42,
        ActiveAgentRun {
            conversation_id: 1,
            control: parent_control,
            gate: parent_gate,
        },
    );
    registry.attach_subagents(42, Arc::clone(&mgr));

    let spawned = mgr
        .dispatch_tool(
            "spawn_subagent",
            r#"{"task":"write doc","access":"workspace_write","wait":false}"#,
        )
        .unwrap();
    assert!(spawned.contains("sub-1"), "got {spawned}");

    let deadline = Instant::now() + Duration::from_secs(5);
    let (call_id, expected_group) = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for child ApprovalRequested event"
        );
        match rx.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok(AgentRunEvent::ApprovalRequested {
                call_id,
                group_key: Some(gk),
                ..
            }) => break (call_id, gk),
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("event channel closed"),
        }
    };

    // 1. Read-only peek sees child pending approval without consuming it.
    assert_eq!(
        registry.pending_scope_info(42, &call_id),
        Some(("write_file".to_string(), Some(expected_group.clone())))
    );

    // 2. Mismatched group:confirm echo fails closed with park intact.
    let mismatch = permissions::resolve_approval(
        &registry,
        &db,
        42,
        &call_id,
        true,
        Some("group:confirm:coding:write_file:wrong"),
    );
    assert_eq!(
        mismatch,
        Err(ApprovalResolveError::Group(
            GroupScopeError::ScopeMismatch {
                expected: expected_group.clone(),
            }
        ))
    );
    assert_eq!(
        registry.pending_scope_info(42, &call_id),
        Some(("write_file".to_string(), Some(expected_group.clone())))
    );

    // 3. Exact group:confirm:<scope> resolves child park and persists allow rule.
    let confirm_arg = format!("group:confirm:{expected_group}");
    permissions::resolve_approval(&registry, &db, 42, &call_id, true, Some(&confirm_arg))
        .expect("group:confirm for child subagent approval must succeed");

    let waited = mgr
        .dispatch_tool("wait_subagent", r#"{"subagent_id":"sub-1"}"#)
        .unwrap();
    assert!(waited.contains("\"status\":\"completed\""), "got {waited}");
    assert_eq!(
        fs::read_to_string(ws.join("docs/note.md")).unwrap(),
        "hello child"
    );

    let expected_path_pattern = permissions::group_path_pattern(Some(&expected_group));
    let persisted = permissions::list_rules(&db).expect("list rules");
    assert!(
        persisted.iter().any(|r| {
            r.preset == "coding"
                && r.tool_pattern == "write_file"
                && r.path_pattern == expected_path_pattern
                && r.effect == RuleEffect::Allow
        }),
        "expected persisted group allow rule for child subagent, got: {persisted:?}"
    );

    mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn send_subagent_message_preserves_monotonic_step_seq_and_clears_stale_result_on_failure() {
    let ws = temp_dir("continuation-seq-result");
    let turn = Arc::new(AtomicUsize::new(0));
    let turn_clone = Arc::clone(&turn);
    let (exec, _) = FnExecutor::new(move |_req, _cancel| {
        let t = turn_clone.fetch_add(1, Ordering::SeqCst);
        match t {
            0 => Ok(tool_reply(vec![make_call(
                "ls-1",
                "list_directory",
                &serde_json::json!({}),
            )])),
            1 => Ok(text_reply("first turn succeeded")),
            // Continuation turn hits max_turns=1 after emitting a model_turn + tool_call.
            _ => Ok(tool_reply(vec![make_call(
                "ls-2",
                "list_directory",
                &serde_json::json!({}),
            )])),
        }
    });

    let (mgr, rx) = make_manager(
        &ws,
        exec,
        RunControl::new(),
        ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard)),
        3,
    );

    let first = mgr
        .dispatch_tool("spawn_subagent", r#"{"task":"initial pass","wait":true}"#)
        .unwrap();
    assert!(first.contains("first turn succeeded"), "got {first}");
    let snap1 = &mgr.snapshots()[0];
    assert_eq!(
        snap1.result_summary.as_deref(),
        Some("first turn succeeded")
    );
    let turn1_seqs: Vec<usize> = snap1.steps.iter().map(|s| s.seq).collect();
    assert_eq!(turn1_seqs, vec![1, 2, 3]);

    // Resumed turn fails via max_turns=1 after emitting more steps.
    let second = mgr
        .dispatch_tool(
            "send_subagent_message",
            r#"{"subagent_id":"sub-1","message":"follow up","max_turns":1,"wait":true}"#,
        )
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&second).expect("valid json");
    let sub = &parsed["subagents"][0];
    assert_eq!(sub["status"], "failed", "got {second}");
    assert!(
        sub["result"].is_null(),
        "failed continuation must clear prior turn's result, got {second}"
    );

    let snap2 = &mgr.snapshots()[0];
    assert_eq!(snap2.status, "failed");
    assert_eq!(
        snap2.result_summary, None,
        "failed continuation must clear prior turn's result_summary"
    );
    let all_seqs: Vec<usize> = snap2.steps.iter().map(|s| s.seq).collect();
    assert_eq!(
        all_seqs,
        vec![1, 2, 3, 4, 5],
        "step seq must increment monotonically across send_subagent_message continuations"
    );

    let streamed_seqs: Vec<usize> = rx
        .try_iter()
        .filter_map(|ev| match ev {
            AgentRunEvent::SubagentStep { seq, .. } => Some(seq),
            _ => None,
        })
        .collect();
    assert_eq!(streamed_seqs, vec![1, 2, 3, 4, 5]);

    mgr.shutdown_and_join();
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn handle_spawn_and_propagate_mode_never_leave_child_above_parent_mode() {
    let ws = temp_dir("spawn-mode-race");
    for _ in 0..25 {
        let (exec, _) = FnExecutor::new(|_req, _cancel| Ok(text_reply("ok")));
        let parent_gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard));
        let (mgr, _rx) = make_manager(&ws, exec, RunControl::new(), parent_gate.clone(), 3);

        let barrier = Arc::new(Barrier::new(2));
        let b_mode = Arc::clone(&barrier);
        let mgr_mode = Arc::clone(&mgr);
        let gate_mode = parent_gate.clone();
        let mode_thread = std::thread::spawn(move || {
            b_mode.wait();
            gate_mode.set_mode(AutonomyMode::Supervised);
            mgr_mode.propagate_mode(AutonomyMode::Supervised);
        });

        barrier.wait();
        let out = mgr
            .dispatch_tool("spawn_subagent", r#"{"task":"race check","wait":false}"#)
            .unwrap();
        assert!(out.contains("sub-1"), "got {out}");
        mode_thread.join().unwrap();

        assert_eq!(parent_gate.mode(), AutonomyMode::Supervised);
        assert_eq!(
            mgr.child_mode_for_test("sub-1"),
            Some(AutonomyMode::Supervised),
            "child autonomy mode must never remain above parent mode after concurrent set_mode"
        );
        mgr.shutdown_and_join();
    }
    let _ = fs::remove_dir_all(&ws);
}
