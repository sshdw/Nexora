//! Iteration budget and spend-limit guard (split from `runner`).

use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::application::agent::control::{AgentRunEvent, RunControl};
use crate::application::agent::pricing;
use crate::application::execution::TokenUsage;

use super::dispatch::emit;
use super::errors::AgentError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default upper bound on consecutive model turns executed by one run.
///
/// A repeatedly tool-calling model cannot loop forever: after this many
/// iterations the run terminates deterministically with
/// [`AgentError::BudgetExhausted`] (AC-9). This is the fixed base bound;
/// adaptive budgets extend it via [`RunControl::extend_steps`] (Task 3.2).
pub(crate) const DEFAULT_MAX_ITERATIONS: usize = 10;

/// Default wall-clock timeout bound applied to each blocking provider
/// request emitted by the runner (Task 3.2).
///
/// The blocking `reqwest` client cannot be interrupted mid-flight, so the
/// honest bound for "terminate running LLM HTTP requests" is a per-request
/// timeout. The provider-independent [`AiRequest`] carries
/// `request_timeout: Option<Duration>`; the runner always sets it to this
/// default unless overridden via [`AgentRunner::with_request_timeout`].
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::new(120, 0);

/// Honour the step budget at this boundary.
///
/// With a control attached, exhaustion parks the loop on
/// `wait_for_allowance` until `extend_steps` continues it or `cancel`
/// aborts it (`resume` alone grants no steps). Without a control, the
/// Task 3.1 deterministic behaviour is preserved: exhaustion returns
/// `AgentError::BudgetExhausted` immediately.
pub(crate) fn honor_allowance(
    control: Option<&RunControl>,
    base: usize,
    taken: usize,
    sender: Option<&Sender<AgentRunEvent>>,
) -> Result<(), AgentError> {
    let Some(c) = control else {
        if taken >= base {
            return Err(AgentError::BudgetExhausted(base));
        }
        return Ok(());
    };
    let allowance = c.allowance(base);
    if taken < allowance {
        return Ok(());
    }
    emit(
        sender,
        AgentRunEvent::BudgetExhausted {
            max_steps: allowance,
        },
    );
    if c.wait_for_allowance(base, taken) {
        Ok(())
    } else {
        emit(sender, AgentRunEvent::Cancelled);
        Err(AgentError::Cancelled)
    }
}

/// Accumulate one turn's billed cost and trip the spend guard (Task 4.3).
///
/// Usage absent is counted as $0 (count-as-known). Known-free model IDs
/// bill $0 regardless of usage.
pub(crate) fn check_spend_guard(
    model: &str,
    usage: Option<TokenUsage>,
    spend_limit_micro_usd: Option<u64>,
    record_present: bool,
    spent_micro_usd: &mut u64,
    sender: Option<&Sender<AgentRunEvent>>,
) -> Result<(), AgentError> {
    if let Some(usage) = usage {
        if spend_limit_micro_usd.is_some() || record_present {
            let cost = pricing::cost_for_model_usage(model, usage);
            *spent_micro_usd = spent_micro_usd.saturating_add(cost);
            if let Some(limit) = spend_limit_micro_usd {
                if *spent_micro_usd > limit {
                    emit(
                        sender,
                        AgentRunEvent::SpendLimitExceeded {
                            spent_micro: *spent_micro_usd,
                            limit_micro: limit,
                        },
                    );
                    return Err(AgentError::SpendLimitExceeded {
                        spent_micro: *spent_micro_usd,
                        limit_micro: limit,
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::persistence::RunRecorder;
    use crate::application::agent::runner::test_support::*;
    use crate::application::agent::runner::AgentRunner;
    use crate::application::execution::AiResponse;
    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;
    use std::fs;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::channel;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn iteration_budget_exhaustion_terminates_deterministically() {
        let ws = temp_workspace();
        // Always demands another tool call: would loop forever unbounded.
        let step = || {
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![call_tool("loop", "list_directory", serde_json::json!({}))],
                usage: None,
            })
        };
        let fake = FakeExecutor::new(vec![step(), step(), step()]);
        let runner = AgentRunner::new(&fake, &ws).with_max_iterations(3);

        let err = runner
            .run("openai", "m", "cred", "loop")
            .expect_err("must exhaust");
        match err {
            AgentError::BudgetExhausted(3) => {}
            other => panic!("expected BudgetExhausted(3), got: {other:?}"),
        }
        assert_eq!(fake.requests.borrow().len(), 3);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn zero_iteration_budget_terminates_before_any_model_turn() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("never reached"))]);
        let runner = AgentRunner::new(&fake, &ws).with_max_iterations(0);

        assert!(matches!(
            runner
                .run("openai", "m", "cred", "q")
                .expect_err("exhausted"),
            AgentError::BudgetExhausted(0)
        ));
        assert!(fake.requests.borrow().is_empty());
        let _ = fs::remove_dir_all(&ws);
    }

    // -----------------------------------------------------------------------
    // Step governor & cancellation (Task 3.2)
    // -----------------------------------------------------------------------

    #[test]
    fn control_activity_is_seen_by_attached_run_control() {
        // The handle is cheaply cloneable and every clone governs the same
        // underlying state.
        let control = RunControl::new();
        let other = control.clone();
        other.extend_steps(4);
        assert_eq!(control.extra_steps(), 4);
        assert_eq!(control.allowance(10), 14);
        assert!(!control.is_cancelled());
        other.cancel();
        assert!(control.is_cancelled());
    }

    #[test]
    fn fixed_budget_without_control_still_hard_stops_deterministically() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(tool_step("a")),
            Ok(tool_step("b")),
            Ok(text_response("later")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_max_iterations(2);
        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must exhaust");
        assert!(matches!(err, AgentError::BudgetExhausted(2)));
        // Exactly the fixed allowance ran; no silent continuation.
        assert_eq!(fake.requests.borrow().len(), 2);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn exhausted_budget_extend_continues_then_completes() {
        let ws = temp_workspace();
        let control = RunControl::new();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(tool_step("a")),
            Ok(tool_step("b")),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(2)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let first = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            control.extend_steps(1);
            let done = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (first, done)
        });

        let answer = runner.run("openai", "m", "cred", "go").expect("completes");
        let (first, done) = driver.join().expect("driver joins");
        assert_eq!(
            first,
            AgentRunEvent::BudgetExhausted { max_steps: 2 },
            "first governance event must be the exhaustion"
        );
        assert_eq!(done, AgentRunEvent::Completed { steps: 3 });
        assert_eq!(answer, "done");
        assert_eq!(fake.requests.borrow().len(), 3);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn exhausted_budget_cancel_aborts_with_cancelled() {
        let ws = temp_workspace();
        let control = RunControl::new();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![Ok(tool_step("a"))]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(1)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let first = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            control.cancel();
            let second = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (first, second)
        });

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must cancel");
        let (first, second) = driver.join().expect("driver joins");
        assert_eq!(first, AgentRunEvent::BudgetExhausted { max_steps: 1 });
        assert_eq!(second, AgentRunEvent::Cancelled);
        assert!(matches!(err, AgentError::Cancelled));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn resume_does_not_end_an_exhausted_budget_wait() {
        let ws = temp_workspace();
        let control = RunControl::new();
        let fake = FakeExecutor::new(vec![Ok(tool_step("a")), Ok(text_response("step"))]);
        let (tx, rx) = channel();
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(1)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let first = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            // resume() alone must NOT grant any steps while parked over budget.
            control.resume();
            // Still parked: no further event (=> no Completed) unless extended.
            let stale = rx.recv_timeout(Duration::from_millis(300));
            assert!(
                stale.is_err(),
                "resume() must not unpark an exhausted-budget wait: got {stale:?}"
            );
            control.extend_steps(1);
            let done = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (first, done)
        });

        let answer = runner
            .run("openai", "m", "cred", "q")
            .expect("completes after extension");
        let (first, done) = driver.join().expect("driver joins");
        assert_eq!(first, AgentRunEvent::BudgetExhausted { max_steps: 1 });
        assert_eq!(done, AgentRunEvent::Completed { steps: 2 });
        assert_eq!(answer, "step");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn pause_then_resume_mid_run_emits_events_and_continues() {
        let ws = temp_workspace();
        let control = RunControl::new();
        // Pause before the run starts: it will park at the very first step
        // boundary.
        control.pause();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(tool_step("a")),
            Ok(tool_step("b")),
            Ok(text_response("ok")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(3)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let paused = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            control.resume();
            let resumed = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            let completed = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (paused, resumed, completed)
        });

        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        let (paused, resumed, completed) = driver.join().expect("driver joins");
        assert_eq!(paused, AgentRunEvent::Paused);
        assert_eq!(resumed, AgentRunEvent::Resumed);
        assert_eq!(completed, AgentRunEvent::Completed { steps: 3 });
        assert_eq!(answer, "ok");
        assert_eq!(fake.requests.borrow().len(), 3);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn cancelling_while_paused_wakes_the_loop_without_deadlock() {
        let ws = temp_workspace();
        let control = RunControl::new();
        control.pause();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![Ok(text_response("never"))]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let paused = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            control.cancel();
            let cancelled = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (paused, cancelled)
        });

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must cancel");
        let (paused, cancelled) = driver.join().expect("driver joins");
        assert_eq!(paused, AgentRunEvent::Paused);
        assert_eq!(cancelled, AgentRunEvent::Cancelled);
        assert!(matches!(err, AgentError::Cancelled));
        // Zero model turns: nothing ran after the parked pause.
        assert!(fake.requests.borrow().is_empty());
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn cancellation_between_provider_calls_stops_further_work() {
        let ws = temp_workspace();
        let control = RunControl::new();
        let (tx, rx) = channel();
        let (gated, entered, release) = GatedExecutor::new(
            vec![
                // Turn 1: dispatch one real workspace write.
                Ok(AiResponse {
                    content: String::new(),
                    model: "m".to_string(),
                    tool_calls: vec![call_tool(
                        "w1",
                        "write_file",
                        serde_json::json!({ "path": "a.txt", "content": "1" }),
                    )],
                    usage: None,
                }),
                // Turn 2 (block_at = 1): parks until the test cancels, then
                // yields a fresh tool call that must never run.
                Ok(AiResponse {
                    content: String::new(),
                    model: "m".to_string(),
                    tool_calls: vec![call_tool(
                        "w2",
                        "write_file",
                        serde_json::json!({ "path": "b.txt", "content": "2" }),
                    )],
                    usage: None,
                }),
                Ok(text_response("never")),
            ],
            1,
        );
        let runner = AgentRunner::new(&gated, &ws)
            .with_max_iterations(3)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            // Wait until the second provider call is actually mid-flight.
            wait_flag(&entered);
            control.cancel();
            // Release the blocked second call so the runner can observe cancel.
            release.store(true, Ordering::SeqCst);
            // The runner aborts with the Cancelled event.
            rx.recv_timeout(Duration::from_secs(5)).expect("event")
        });

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must cancel");
        // Cancellation surfaces after the in-flight call returns; the pending
        // tool call of turn 2 must never be dispatched.
        let cancelled = driver.join().expect("driver joins");
        assert_eq!(cancelled, AgentRunEvent::Cancelled);
        assert!(matches!(err, AgentError::Cancelled));
        assert_eq!(gated.requests.borrow().len(), 2, "only two LLM turns ran");
        assert!(
            fs::read_to_string(ws.join("a.txt")).is_ok(),
            "turn-1 tool ran"
        );
        assert!(
            fs::read_to_string(ws.join("b.txt")).is_err(),
            "turn-2 tool must not run after cancellation"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn runner_requests_carry_default_request_timeout_and_override_flows_through() {
        let ws = temp_workspace();
        let default_fake = FakeExecutor::new(vec![Ok(text_response("ok"))]);
        let default_runner = AgentRunner::new(&default_fake, &ws);
        default_runner
            .run("openai", "m", "cred", "q")
            .expect("finish");
        let captured = &default_fake.requests.borrow()[0];
        assert_eq!(
            captured.request_timeout,
            Some(DEFAULT_REQUEST_TIMEOUT),
            "runner applies its configurable default timeout to every request"
        );
        let _ = fs::remove_dir_all(&ws);

        // A custom timeout overrides it.
        let custom_fake = FakeExecutor::new(vec![Ok(text_response("ok"))]);
        let custom = Duration::from_secs(7);
        let custom_runner = AgentRunner::new(&custom_fake, &ws).with_request_timeout(custom);
        custom_runner
            .run("openai", "m", "cred", "q")
            .expect("finish");
        assert_eq!(
            custom_fake.requests.borrow()[0].request_timeout,
            Some(custom)
        );
        let _ = fs::remove_dir_all(&ws);
    }

    // -----------------------------------------------------------------------
    // Spend guard (Task 4.3)
    // -----------------------------------------------------------------------

    #[test]
    fn spend_guard_trips_exactly_on_exceed() {
        let ws = temp_workspace();
        // Each turn costs 1_000_000 micro (200_000 input tokens * 5_000_000 / 1M)
        let cheap = |id| usage_tool_response(id, 200_000, 0);
        let fake = FakeExecutor::new(vec![
            Ok(cheap("a")),
            Ok(cheap("b")),
            Ok(usage_response("final", 200_000, 0)),
        ]);
        let limit = 2_500_000u64; // 2.5M, so 2*1M=2M under, 3*1M=3M over
        let (tx, rx) = channel();
        let runner = AgentRunner::new(&fake, &ws)
            .with_spend_limit(limit)
            .with_event_sender(tx);
        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must trip");
        match err {
            AgentError::SpendLimitExceeded {
                spent_micro,
                limit_micro,
            } => {
                assert_eq!(spent_micro, 3_000_000);
                assert_eq!(limit_micro, limit);
            }
            other => panic!("expected SpendLimitExceeded, got {other:?}"),
        }
        // Event payload correct
        let ev = rx.recv_timeout(Duration::from_secs(2)).expect("event");
        assert_eq!(
            ev,
            AgentRunEvent::SpendLimitExceeded {
                spent_micro: 3_000_000,
                limit_micro: limit
            }
        );
        // Two tool calls ran (first two turns), third was final but tripped before return
        assert_eq!(fake.requests.borrow().len(), 3);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_no_limit_behaves_identical() {
        let ws = temp_workspace();
        // Same script as above, but no limit вЂ” must complete normally
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![call_tool("a", "list_directory", serde_json::json!({}))],
                usage: Some(TokenUsage {
                    input_tokens: 200_000,
                    output_tokens: 0,
                }),
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);
        let ans = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(ans, "done");
        assert_eq!(fake.requests.borrow().len(), 2);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_usage_none_adds_zero() {
        let ws = temp_workspace();
        // First turn: usage None (cost 0), second: cheap 1M, limit 500k -> second trips
        // Actually first None adds 0, spent 0, second 1M >500k trips
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![call_tool("a", "list_directory", serde_json::json!({}))],
                usage: None,
            }),
            Ok(usage_tool_response("b", 200_000, 0)),
            Ok(text_response("never")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_spend_limit(500_000);
        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("trips on second");
        assert!(matches!(err, AgentError::SpendLimitExceeded { .. }));
        // Only 2 turns ran (first None + second that tripped)
        assert_eq!(fake.requests.borrow().len(), 2);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_event_and_error_payload_correct() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(usage_response("hi", 400_000, 0))]);
        // 400k *5M/1M =2_000_000
        let limit = 1_000_000u64;
        let (tx, rx) = channel();
        let runner = AgentRunner::new(&fake, &ws)
            .with_spend_limit(limit)
            .with_event_sender(tx);
        let err = runner.run("openai", "m", "cred", "q").expect_err("trips");
        match &err {
            AgentError::SpendLimitExceeded {
                spent_micro,
                limit_micro,
            } => {
                assert_eq!(*spent_micro, 2_000_000);
                assert_eq!(*limit_micro, limit);
                // Display contains integers, no secrets
                let s = format!("{err}");
                assert!(s.contains("2000000"));
                assert!(s.contains("1000000"));
            }
            _ => panic!("wrong error"),
        }
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match ev {
            AgentRunEvent::SpendLimitExceeded {
                spent_micro,
                limit_micro,
            } => {
                assert_eq!(spent_micro, 2_000_000);
                assert_eq!(limit_micro, limit);
            }
            _ => panic!("wrong event"),
        }
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_recorder_persists_status_and_spend() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(usage_response("hi", 400_000, 0))]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_spend_limit(1_000_000)
            .with_run_recorder(RunRecorder::new(&db));
        let err = runner.run("openai", "m", "cred", "q").expect_err("trips");
        assert!(matches!(err, AgentError::SpendLimitExceeded { .. }));
        let runs = AgentRunRepository::new(&db);
        let run = runs.list_runs_by_started_at_desc().expect("list")[0].clone();
        assert_eq!(run.status, "spend_limit_exceeded");
        assert_eq!(run.spent_micro_usd, Some(2_000_000));
        assert_eq!(run.limit_micro_usd, Some(1_000_000));
        assert_eq!(run.error, None);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_non_recorded_still_emits_event() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(usage_response("hi", 400_000, 0))]);
        let (tx, rx) = channel();
        let runner = AgentRunner::new(&fake, &ws)
            .with_spend_limit(1_000_000)
            .with_event_sender(tx);
        let err = runner.run("openai", "m", "cred", "q").expect_err("trips");
        assert!(matches!(err, AgentError::SpendLimitExceeded { .. }));
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(ev, AgentRunEvent::SpendLimitExceeded { .. }));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_step_governor_untouched() {
        let ws = temp_workspace();
        // BudgetExhausted should still happen when max_iterations hit, even with a spend limit that is not tripped
        let fake = FakeExecutor::new(vec![
            Ok(usage_tool_response("a", 10, 0)), // cost tiny 50 micro
            Ok(usage_tool_response("b", 10, 0)),
            Ok(text_response("later")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(2)
            .with_spend_limit(10_000_000); // high, not tripped
        let err = runner.run("openai", "m", "cred", "q").expect_err("budget");
        assert!(matches!(err, AgentError::BudgetExhausted(2)));
        assert_eq!(fake.requests.borrow().len(), 2);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_exactly_at_limit_does_not_trip() {
        let ws = temp_workspace();
        // Cost 1M per turn, limit 2M, two turns exactly at limit -> should complete
        let fake = FakeExecutor::new(vec![
            Ok(usage_tool_response("a", 200_000, 0)),
            Ok(usage_response("done", 200_000, 0)),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_spend_limit(2_000_000);
        let ans = runner
            .run("openai", "m", "cred", "q")
            .expect("at limit completes");
        assert_eq!(ans, "done");
        let _ = fs::remove_dir_all(&ws);
    }
}
