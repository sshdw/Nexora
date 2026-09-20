//! Agent-loop failure classification (split from `runner`).

use crate::application::execution::ExecutorError;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Classified agent-loop failure. Carries no secret payload and never embeds
/// credential material (ARCHITECTURE.md Р’В§9, Р’В§11): the provider variant wraps
/// the already-classified [`ExecutorError`].
#[derive(Debug)]
pub(crate) enum AgentError {
    /// The provider failed to fulfil one of the loop's requests. The
    /// classified [`ExecutorError`] passes through verbatim to the run
    /// error text (its Display is rendered unchanged).
    Provider(ExecutorError),
    /// The iteration budget was exhausted before the model produced a final
    /// answer. With no [`RunControl`] attached this aborts outright; with one
    /// attached the run first parked at the boundary awaiting `extend_steps`
    /// and only aborts if it was instead cancelled.
    BudgetExhausted(usize),
    /// The spend guard tripped: billed spend exceeded the configured per-run
    /// limit (Task 4.3). `spent_micro` includes the tripping turn's cost.
    SpendLimitExceeded { spent_micro: u64, limit_micro: u64 },
    /// The provider returned neither tool calls nor usable final content.
    EmptyResponse,
    /// A user cancelled the run via [`RunControl::cancel`] (or cancellation
    /// was observed during a tool execution).
    Cancelled,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(err) => write!(f, "{err}"),
            Self::BudgetExhausted(max) => write!(
                f,
                "agent stopped: reached the {max}-step limit without a final answer"
            ),
            Self::SpendLimitExceeded {
                spent_micro,
                limit_micro,
            } => write!(
                f,
                "agent stopped: spend limit exceeded (spent {spent_micro} micro-USD of {limit_micro} micro-USD)"
            ),
            Self::EmptyResponse => {
                write!(f, "agent stopped: the model returned an empty response")
            }
            Self::Cancelled => write!(f, "agent stopped: cancelled by the user"),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Provider(err) => Some(err),
            Self::BudgetExhausted(_)
            | Self::SpendLimitExceeded { .. }
            | Self::EmptyResponse
            | Self::Cancelled => None,
        }
    }
}

impl From<ExecutorError> for AgentError {
    fn from(err: ExecutorError) -> Self {
        // A provider call cancelled in flight is user cancellation, not a
        // provider failure: it maps to `Cancelled` exactly like a park-cancel,
        // so the terminal outcome, the `Cancelled` governance event, and the
        // persisted `cancelled` status all agree.
        if matches!(err, ExecutorError::Cancelled) {
            return Self::Cancelled;
        }
        Self::Provider(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::runner::test_support::*;
    use crate::application::agent::runner::AgentRunner;
    use crate::application::execution::AiResponse;
    use crate::application::execution::ExecutorError;
    use std::fs;

    #[test]
    fn provider_failure_is_propagated_as_classified_error() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Err(ExecutorError::Failure)]);
        let runner = AgentRunner::new(&fake, &ws);

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("provider failed");
        assert!(matches!(err, AgentError::Provider(_)));
        // Exactly one attempt: failures are not retried here.
        assert_eq!(fake.requests.borrow().len(), 1);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn mid_loop_provider_failure_leaves_no_partial_success() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![call_tool("t", "list_directory", serde_json::json!({}))],
                usage: None,
            }),
            Err(ExecutorError::Failure),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("second turn fails");
        assert!(matches!(err, AgentError::Provider(_)));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn empty_text_without_tool_calls_is_a_controlled_failure() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("   "))]);
        let runner = AgentRunner::new(&fake, &ws);

        assert!(matches!(
            runner
                .run("openai", "m", "cred", "q")
                .expect_err("empty answer"),
            AgentError::EmptyResponse
        ));
        let _ = fs::remove_dir_all(&ws);
    }
}
