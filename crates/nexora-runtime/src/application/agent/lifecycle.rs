//! Agent run lifecycle: explicit state machine plus input/output contracts
//! (WS-B.1 foundation).
//!
//! Codifies the run states implicit in the current implementation —
//! [`crate::application::agent::runner`] (the `ReAct` loop), [`super::service`]
//! (claim → row → spawn), [`super::persistence`] (`agent_runs.status`
//! strings), [`super::errors::AgentError`], and the `agent-run-event` channel
//! ([`super::control::AgentRunEvent`], bridged by `commands/agent.rs` and
//! `src/lib/tauri.ts`) — so later WS-B (budgets, gates, audit) and WS-C
//! (snapshots, checkpoints) work shares one state model.
//!
//! Codify-only: the table covers every evidenced state-change site and
//! nothing more. Any behavior difference found during inspection is a bug in
//! the model, not a license to change behavior.
//!
//! # Evidenced state-change map
//!
//! | From → To | Evidence |
//! |---|---|
//! | `Queued` → `Running` | `service.rs::start_run_claimed` creates the `agent_runs` row (`status='running'`) via `RunRecorder::create_run_row`, registers it, then `spawn_run` starts the run thread. |
//! | `Running` → `Paused` | `dispatch.rs::honor_pause` emits `AgentRunEvent::Paused` when `pause_pending`. |
//! | `Paused` → `Running` | `dispatch.rs::honor_pause` emits `Resumed` when `wait_while_paused` returns true. |
//! | `Paused` → `Cancelled` | `dispatch.rs::honor_pause` emits `Cancelled` when the pause wait is cancelled. |
//! | `Running` → `AwaitingApproval` | `dispatch.rs::park_for_approval` emits `ApprovalRequested` after `prepare_pending_with_group`. |
//! | `AwaitingApproval` → `Running` | `dispatch.rs::park_for_approval` emits `ApprovalResolved`; approved and denied both continue the loop. |
//! | `AwaitingApproval` → `Cancelled` | `dispatch.rs::park_for_approval` maps a cancelled park to `AgentError::Cancelled` and emits `Cancelled`. |
//! | `Running` → `AwaitingBudget` | `budget.rs::honor_allowance` (control attached) emits `BudgetExhausted` then parks on `wait_for_allowance`. |
//! | `AwaitingBudget` → `Running` | `budget.rs::honor_allowance` returns `Ok` when `extend_steps` raises the allowance. |
//! | `AwaitingBudget` → `Cancelled` | `budget.rs::honor_allowance` emits `Cancelled` when the budget wait is cancelled. |
//! | `Running` → `BudgetExhausted` (terminal) | `budget.rs::honor_allowance` (no control) returns `AgentError::BudgetExhausted`; `persistence.rs::terminal_outcome` maps it to `budget_exhausted`. |
//! | `Running` → `Compacting` | `runner.rs::react_loop` emits `CompactionStarted` (`threshold` proactive, `overflow` reactive). |
//! | `Compacting` → `Running` | `runner.rs` emits `CompactionFinished`/`CompactionFailed` and continues (reactive path re-sends the turn). |
//! | `Compacting` → `Cancelled` | `runner.rs` maps `CompactionOutcome::Cancelled` to `AgentError::Cancelled` and emits `Cancelled`. |
//! | `Running` → `Completed` | `runner.rs` emits `Completed` and returns `Ok`; `terminal_outcome` maps it to `completed`. |
//! | `Running` → `Cancelled` | `dispatch.rs::check_cancellation`, the post-provider check in `runner.rs`, and the `ExecutorError::Cancelled` arm all emit `Cancelled`. |
//! | `Running` → `SpendLimitExceeded` (terminal) | `budget.rs::check_spend_guard` emits `SpendLimitExceeded`; `terminal_outcome` maps it to `spend_limit_exceeded`. |
//! | `Running` → `Failed` (terminal, persisted as `error`) | `terminal_outcome` maps `EmptyResponse` / `Provider` / `ContextExhausted` to `error`. |
//!
//! Permission-store (#65) and routing-profile (#69) gates fire inside the
//! per-tool-call dispatch (`dispatch.rs`), i.e. while the run is `Running`
//! (or parked in `AwaitingApproval`); the lifecycle describes where they fire
//! without changing their behavior.

use serde::{Deserialize, Serialize};

use crate::application::routing::{profile_key, TaskKind};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Explicit agent run lifecycle state (WS-B.1).
///
/// Transient parks (`Paused`, `AwaitingApproval`, `AwaitingBudget`,
/// `Compacting`) keep the persisted `agent_runs.status = 'running'`; only
/// `Running` and the five terminals have a distinct persisted status (see
/// [`RunState::persisted_status`]). `Failed` serializes as `"error"` to match
/// the persisted `agent_runs.status` vocabulary (`terminal_outcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunState {
    /// Conversation claimed, run row not yet created (`start_run_claimed`
    /// pre-spawn window). No persisted row yet.
    Queued,
    /// The run thread is executing the `ReAct` loop (`agent_runs` `running`).
    Running,
    /// Parked at a step boundary by `pause()` (`Paused` event).
    Paused,
    /// Parked on a per-tool-call approval (`ApprovalRequested` event).
    AwaitingApproval,
    /// Parked at an exhausted step budget (`BudgetExhausted` event).
    AwaitingBudget,
    /// Folding older turns into a summary (`CompactionStarted` event).
    Compacting,
    /// Terminal success (`completed`).
    Completed,
    /// Terminal user cancellation (`cancelled`).
    Cancelled,
    /// Terminal step-budget exhaustion without a control (`budget_exhausted`).
    BudgetExhausted,
    /// Terminal spend-guard trip (`spend_limit_exhausted`).
    SpendLimitExceeded,
    /// Terminal classified failure, persisted as `error` (`terminal_outcome`
    /// `EmptyResponse` / `Provider` / `ContextExhausted` arm).
    #[serde(rename = "error")]
    Failed,
}

impl RunState {
    /// Canonical state name. `Failed` reports `"error"` to match the
    /// persisted status and `RunFinished` vocabulary.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::AwaitingApproval => "awaiting_approval",
            Self::AwaitingBudget => "awaiting_budget",
            Self::Compacting => "compacting",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::BudgetExhausted => "budget_exhausted",
            Self::SpendLimitExceeded => "spend_limit_exceeded",
            Self::Failed => "error",
        }
    }

    /// Whether the state is terminal (no exit transitions).
    #[must_use]
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Cancelled
                | Self::BudgetExhausted
                | Self::SpendLimitExceeded
                | Self::Failed
        )
    }

    /// Persisted `agent_runs.status` while in this state, or `None` when no
    /// row exists yet (`Queued`). Transient parks keep `'running'`.
    #[must_use]
    pub(crate) const fn persisted_status(self) -> Option<&'static str> {
        match self {
            Self::Queued => None,
            Self::Running
            | Self::Paused
            | Self::AwaitingApproval
            | Self::AwaitingBudget
            | Self::Compacting => Some("running"),
            Self::Completed => Some("completed"),
            Self::Cancelled => Some("cancelled"),
            Self::BudgetExhausted => Some("budget_exhausted"),
            Self::SpendLimitExceeded => Some("spend_limit_exceeded"),
            Self::Failed => Some("error"),
        }
    }

    /// Existing `agent-run-event` payload vocabulary entered on this state,
    /// or `None` when entry emits no governance event. No channel changes:
    /// names match the `AgentRunEvent` variants and the
    /// `RunFinished.status` strings. The terminal `BudgetExhausted` shares
    /// the park vocabulary (`budget_exhausted`); the terminal itself surfaces
    /// via the `Finished` frame, exactly as today.
    #[must_use]
    pub(crate) const fn entry_event_name(self) -> Option<&'static str> {
        match self {
            Self::Queued | Self::Running | Self::Failed => None,
            Self::Paused => Some("paused"),
            Self::AwaitingApproval => Some("approval_requested"),
            Self::AwaitingBudget | Self::BudgetExhausted => Some("budget_exhausted"),
            Self::Compacting => Some("compaction_started"),
            Self::Completed => Some("completed"),
            Self::Cancelled => Some("cancelled"),
            Self::SpendLimitExceeded => Some("spend_limit_exceeded"),
        }
    }
}

// ---------------------------------------------------------------------------
// Transition
// ---------------------------------------------------------------------------

/// Secret-free illegal-transition error: carries only the fixed state
/// vocabulary, never ids, credentials, SQL, or message content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LifecycleError {
    from: &'static str,
    to: &'static str,
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "illegal agent run transition from '{}' to '{}'",
            self.from, self.to
        )
    }
}

impl std::error::Error for LifecycleError {}

/// Check one lifecycle transition against the evidenced table.
///
/// Returns `Ok(())` for legal transitions and a secret-free
/// [`LifecycleError`] otherwise. Terminal states have no exit transitions.
pub(crate) const fn transition(from: RunState, to: RunState) -> Result<(), LifecycleError> {
    let legal = matches!(
        (from, to),
        (RunState::Queued, RunState::Running)
            | (
                RunState::Running,
                RunState::Paused
                    | RunState::AwaitingApproval
                    | RunState::AwaitingBudget
                    | RunState::Compacting
                    | RunState::Completed
                    | RunState::Cancelled
                    | RunState::BudgetExhausted
                    | RunState::SpendLimitExceeded
                    | RunState::Failed,
            )
            | (
                RunState::Paused
                    | RunState::AwaitingApproval
                    | RunState::AwaitingBudget
                    | RunState::Compacting,
                RunState::Running | RunState::Cancelled,
            )
    );
    if legal {
        Ok(())
    } else {
        Err(LifecycleError {
            from: from.as_str(),
            to: to.as_str(),
        })
    }
}

/// Consult the transition table at an existing state-change point without
/// changing behavior: legal transitions are no-ops; illegal ones log (and
/// `debug_assert`) but never alter the run's outcome.
pub(crate) fn observe_transition(from: RunState, to: RunState) {
    if transition(from, to).is_err() {
        log::warn!(
            "agent run lifecycle: illegal transition from '{}' to '{}'",
            from.as_str(),
            to.as_str()
        );
        debug_assert!(
            transition(from, to).is_ok(),
            "illegal agent run transition from '{}' to '{}'",
            from.as_str(),
            to.as_str()
        );
    }
}

// ---------------------------------------------------------------------------
// Contracts (pure data)
// ---------------------------------------------------------------------------

/// Which routing profile a run serves. Values match
/// [`crate::application::routing::TaskKind`] (`chat` / `agent`) and their
/// settings keys; serialized response-side (never IPC args).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskKey {
    Chat,
    Agent,
}

impl TaskKey {
    /// Canonical key (`chat` / `agent`).
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Agent => "agent",
        }
    }

    /// Settings key holding this task's routing profile
    /// (`routing.profile.chat` / `routing.profile.agent`).
    #[must_use]
    pub(crate) const fn profile_key(self) -> &'static str {
        match self {
            Self::Chat => crate::application::routing::CHAT_PROFILE_KEY,
            Self::Agent => crate::application::routing::AGENT_PROFILE_KEY,
        }
    }
}

impl From<TaskKind> for TaskKey {
    fn from(kind: TaskKind) -> Self {
        match kind {
            TaskKind::Chat => Self::Chat,
            TaskKind::Agent => Self::Agent,
        }
    }
}

impl From<TaskKey> for TaskKind {
    fn from(key: TaskKey) -> Self {
        match key {
            TaskKey::Chat => Self::Chat,
            TaskKey::Agent => Self::Agent,
        }
    }
}

/// Resolved model reference: provider/model identifiers only. Carries no
/// credential (keyring-only, never across IPC, never in logs or errors).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelRef {
    /// Internal provider name.
    pub provider: String,
    /// Model identifier within the provider.
    pub model: String,
}

/// Opaque budget handles for WS-B.3. Placeholders only: no logic reads them
/// yet, and attaching them changes no behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct BudgetHandles {
    /// Iteration-budget override (`None` = runner default).
    pub max_iterations: Option<usize>,
    /// Spend-limit override in micro-USD (`None` = no guard).
    pub spend_limit_micro_usd: Option<u64>,
}

/// Validated run input contract (pure data): who runs what under which
/// budget. The credential is resolved separately inside the backend and never
/// enters this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunInput {
    /// Owning conversation.
    pub conversation_id: i64,
    /// Task key selecting the routing profile.
    pub task_key: TaskKey,
    /// Resolved provider/model reference.
    pub model: ModelRef,
    /// Opaque budget handles (WS-B.3 placeholders).
    pub budget: BudgetHandles,
}

/// Run output contract (pure data, response-side): identity, resolution,
/// terminal state, and Unix-seconds timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunOutput {
    /// `agent_runs.id`.
    pub run_id: i64,
    /// Task key the run served.
    pub task_key: TaskKey,
    /// Resolved provider/model reference.
    pub model: ModelRef,
    /// Opaque budget handles echoed from the input (WS-B.3 placeholders).
    pub budget: BudgetHandles,
    /// Terminal lifecycle state.
    pub state: RunState,
    /// Persisted `agent_runs.status` string (`completed` / `cancelled` /
    /// `budget_exhausted` / `spend_limit_exceeded` / `error`).
    pub status: String,
    /// Start timestamp, Unix seconds.
    pub started_at: i64,
    /// Termination timestamp, Unix seconds (`None` while active).
    pub finished_at: Option<i64>,
}

/// Silence the unused-import lint when routing keys are referenced through
/// the `TaskKey::profile_key` const path only in docs; the import above is
/// load-bearing for the `From` impls.
#[allow(clippy::single_component_path_imports)]
fn _assert_profile_key_fn() {
    let _ = profile_key;
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [RunState; 11] = [
        RunState::Queued,
        RunState::Running,
        RunState::Paused,
        RunState::AwaitingApproval,
        RunState::AwaitingBudget,
        RunState::Compacting,
        RunState::Completed,
        RunState::Cancelled,
        RunState::BudgetExhausted,
        RunState::SpendLimitExceeded,
        RunState::Failed,
    ];

    // Exactly the 18 evidenced transitions. Queued has a single evidenced
    // exit; setup failures before the row exists stay outside the machine
    // (claim released, no terminal row — see `queued_has_single_exit`).
    const LEGAL: [(RunState, RunState); 18] = [
        (RunState::Running, RunState::Paused),
        (RunState::Running, RunState::AwaitingApproval),
        (RunState::Running, RunState::AwaitingBudget),
        (RunState::Running, RunState::Compacting),
        (RunState::Running, RunState::Completed),
        (RunState::Running, RunState::Cancelled),
        (RunState::Running, RunState::BudgetExhausted),
        (RunState::Running, RunState::SpendLimitExceeded),
        (RunState::Running, RunState::Failed),
        (RunState::Paused, RunState::Running),
        (RunState::Paused, RunState::Cancelled),
        (RunState::AwaitingApproval, RunState::Running),
        (RunState::AwaitingApproval, RunState::Cancelled),
        (RunState::AwaitingBudget, RunState::Running),
        (RunState::AwaitingBudget, RunState::Cancelled),
        (RunState::Compacting, RunState::Running),
        (RunState::Compacting, RunState::Cancelled),
        (RunState::Queued, RunState::Running),
    ];

    #[test]
    fn transition_table_exhaustive_legal_and_illegal() {
        // Every table entry is legal ...
        let mut seen = std::collections::HashSet::new();
        for (from, to) in &LEGAL[..18] {
            assert!(
                transition(*from, *to).is_ok(),
                "table entry {from:?} -> {to:?} must be legal"
            );
            seen.insert((*from, *to));
        }
        // ... and every other pair is illegal with a secret-free error.
        for from in ALL {
            for to in ALL {
                if seen.contains(&(from, to)) {
                    continue;
                }
                // Self-transitions are never legal: parks re-enter via
                // explicit resume/resolve/extend events, not silent loops.
                let err =
                    transition(from, to).expect_err(&format!("{from:?} -> {to:?} must be illegal"));
                let rendered = format!("{err}");
                assert!(
                    rendered.contains(from.as_str()) && rendered.contains(to.as_str()),
                    "error names both states, got {rendered:?}"
                );
                for sentinel in ["sk-", "secret", "credential", "api_key", "SELECT"] {
                    assert!(
                        !rendered.to_lowercase().contains(sentinel),
                        "transition error must stay secret-free, found {sentinel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn queued_has_single_exit() {
        let exits: Vec<RunState> = ALL
            .into_iter()
            .filter(|to| transition(RunState::Queued, *to).is_ok())
            .collect();
        assert_eq!(exits, vec![RunState::Running]);
    }

    #[test]
    fn terminal_states_pin_no_exits() {
        for terminal in [
            RunState::Completed,
            RunState::Cancelled,
            RunState::BudgetExhausted,
            RunState::SpendLimitExceeded,
            RunState::Failed,
        ] {
            assert!(terminal.is_terminal(), "{terminal:?} must be terminal");
            for to in ALL {
                assert!(
                    transition(terminal, to).is_err(),
                    "terminal {terminal:?} must have no exit, got -> {to:?}"
                );
            }
        }
        for live in [
            RunState::Queued,
            RunState::Running,
            RunState::Paused,
            RunState::AwaitingApproval,
            RunState::AwaitingBudget,
            RunState::Compacting,
        ] {
            assert!(!live.is_terminal(), "{live:?} must not be terminal");
        }
    }

    #[test]
    fn no_orphan_states_no_dead_transitions() {
        // Every state is reachable and (unless terminal) can leave.
        for state in ALL {
            let has_entry = ALL.into_iter().any(|from| transition(from, state).is_ok());
            let has_exit = ALL.into_iter().any(|to| transition(state, to).is_ok());
            // Queued is the initial state: entry-free by construction.
            if state == RunState::Queued {
                assert!(has_exit, "Queued must have an exit");
                continue;
            }
            assert!(has_entry, "{state:?} is orphaned (no entry)");
            if !state.is_terminal() {
                assert!(has_exit, "{state:?} is stuck (no exit)");
            }
        }
        // Every legal transition is the only path it claims: count pins the
        // table against silent additions.
        let mut legal_count = 0;
        for from in ALL {
            for to in ALL {
                if transition(from, to).is_ok() {
                    legal_count += 1;
                }
            }
        }
        assert_eq!(legal_count, 18, "exactly the 18 evidenced transitions");
    }

    #[test]
    fn persisted_status_matches_terminal_outcome_vocabulary() {
        assert_eq!(RunState::Queued.persisted_status(), None);
        for parked in [
            RunState::Running,
            RunState::Paused,
            RunState::AwaitingApproval,
            RunState::AwaitingBudget,
            RunState::Compacting,
        ] {
            assert_eq!(parked.persisted_status(), Some("running"));
        }
        assert_eq!(RunState::Completed.persisted_status(), Some("completed"));
        assert_eq!(RunState::Cancelled.persisted_status(), Some("cancelled"));
        assert_eq!(
            RunState::BudgetExhausted.persisted_status(),
            Some("budget_exhausted")
        );
        assert_eq!(
            RunState::SpendLimitExceeded.persisted_status(),
            Some("spend_limit_exceeded")
        );
        assert_eq!(RunState::Failed.persisted_status(), Some("error"));
    }

    #[test]
    fn entry_events_use_existing_payload_vocabulary() {
        assert_eq!(RunState::Paused.entry_event_name(), Some("paused"));
        assert_eq!(
            RunState::AwaitingApproval.entry_event_name(),
            Some("approval_requested")
        );
        assert_eq!(
            RunState::AwaitingBudget.entry_event_name(),
            Some("budget_exhausted")
        );
        assert_eq!(
            RunState::Compacting.entry_event_name(),
            Some("compaction_started")
        );
        assert_eq!(RunState::Completed.entry_event_name(), Some("completed"));
        assert_eq!(RunState::Cancelled.entry_event_name(), Some("cancelled"));
        assert_eq!(
            RunState::SpendLimitExceeded.entry_event_name(),
            Some("spend_limit_exceeded")
        );
        assert_eq!(RunState::Queued.entry_event_name(), None);
        assert_eq!(RunState::Running.entry_event_name(), None);
        assert_eq!(RunState::Failed.entry_event_name(), None);
    }

    #[test]
    fn task_key_matches_routing_profile_keys() {
        assert_eq!(TaskKey::Chat.as_str(), "chat");
        assert_eq!(TaskKey::Agent.as_str(), "agent");
        assert_eq!(
            TaskKey::Chat.profile_key(),
            crate::application::routing::CHAT_PROFILE_KEY
        );
        assert_eq!(
            TaskKey::Agent.profile_key(),
            crate::application::routing::AGENT_PROFILE_KEY
        );
        assert_eq!(TaskKey::from(TaskKind::Chat), TaskKey::Chat);
        assert_eq!(TaskKey::from(TaskKind::Agent), TaskKey::Agent);
        assert_eq!(TaskKind::from(TaskKey::Chat), TaskKind::Chat);
    }

    #[test]
    fn contract_round_trip_snake_case() {
        let input = RunInput {
            conversation_id: 7,
            task_key: TaskKey::Agent,
            model: ModelRef {
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
            },
            budget: BudgetHandles {
                max_iterations: Some(10),
                spend_limit_micro_usd: Some(250_000),
            },
        };
        let raw = serde_json::to_string(&input).expect("serialize input");
        for key in [
            "conversation_id",
            "task_key",
            "spend_limit_micro_usd",
            "max_iterations",
        ] {
            assert!(raw.contains(key), "input payload keeps {key}, got {raw}");
        }
        for camel in ["conversationId", "taskKey", "spendLimit", "maxIterations"] {
            assert!(
                !raw.contains(camel),
                "input payload must not use camelCase {camel}, got {raw}"
            );
        }
        let back: RunInput = serde_json::from_str(&raw).expect("round-trip input");
        assert_eq!(back, input);

        let output = RunOutput {
            run_id: 3,
            task_key: TaskKey::Agent,
            model: ModelRef {
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
            },
            budget: BudgetHandles {
                max_iterations: None,
                spend_limit_micro_usd: None,
            },
            state: RunState::Completed,
            status: "completed".to_string(),
            started_at: 1_700_000_000,
            finished_at: Some(1_700_000_060),
        };
        let raw = serde_json::to_string(&output).expect("serialize output");
        for key in ["run_id", "task_key", "started_at", "finished_at"] {
            assert!(raw.contains(key), "output payload keeps {key}, got {raw}");
        }
        for camel in ["runId", "taskKey", "startedAt", "finishedAt"] {
            assert!(
                !raw.contains(camel),
                "output payload must not use camelCase {camel}, got {raw}"
            );
        }
        // Failed serializes as the persisted "error" vocabulary.
        let failed = RunOutput {
            state: RunState::Failed,
            status: "error".to_string(),
            ..output.clone()
        };
        let raw = serde_json::to_string(&failed).expect("serialize failed");
        assert!(
            raw.contains("\"error\""),
            "failed uses error vocab, got {raw}"
        );
        let back: RunOutput = serde_json::from_str(&raw).expect("round-trip output");
        assert_eq!(back, failed);

        // Credential material never enters the contracts: the structs have no
        // such field, so serialization cannot leak one.
        for sentinel in ["sk-", "credential"] {
            assert!(!raw.to_lowercase().contains(sentinel));
        }
    }
}
