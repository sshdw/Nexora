use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::application::agent::access::{AccessContext, AccessProfile, PROFILE_KEY};
use crate::application::agent::approval::{ApprovalGate, AutonomyMode};
use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
use crate::application::agent::permissions::{is_subagent_tool, RunPreset};
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
