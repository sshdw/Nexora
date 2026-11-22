//! Task prompt composition (plan → act → verify → report prompts).
//!
//! Split from `tasks.rs` (AC-207): the step prompt builder and the loop
//! report composer live here; the loop itself is a sibling.

use std::fmt::Write as _;

use crate::infrastructure::repository::agent_tasks::AgentTaskStep;

use super::REPORT_EXCERPT_MAX_CHARS;

/// Build one step's agent prompt: the plan → act → verify → report loop in
/// miniature. The step runs inside the task's backing conversation, so prior
/// steps' persisted messages are already in context; `prior` carries only
/// the compacted excerpts of this loop's earlier steps.
pub(super) fn build_step_prompt(
    task_title: &str,
    step_title: &str,
    step_number: usize,
    total_steps: i64,
    prior: &[String],
) -> String {
    let mut prompt = format!(
        "Autonomous task \"{task_title}\" — step {step_number} of {total_steps}: {step_title}.\n\
         Plan the step, act with the available tools, verify the outcome, then report back: \
         end your turn with a concise report of what this step achieved (or what blocked it)."
    );
    if !prior.is_empty() {
        prompt.push_str("\nEarlier steps this run completed:\n");
        for line in prior {
            prompt.push_str(line);
            prompt.push('\n');
        }
    }
    prompt
}

/// Compose the terminal task report: status line plus one line per step
/// (status + excerpt). Fixed vocabulary; step text is the loop's own
/// recorded results.
pub(super) fn compose_report(
    title: &str,
    status: &str,
    steps: &[AgentTaskStep],
    stop_reason: Option<&str>,
    max_steps: usize,
    defined: usize,
) -> String {
    let done = steps.iter().filter(|s| s.status == "completed").count();
    let mut report = format!(
        "Task \"{title}\" — {status}: {done} of {} steps completed.",
        steps.len()
    );
    if let Some(reason) = stop_reason {
        let _ = write!(report, " Loop {reason}.");
    }
    if defined > max_steps {
        let _ = write!(
            report,
            " Step cap: executed {max_steps} of {defined} defined steps."
        );
    }
    for step in steps {
        let excerpt = step
            .result
            .as_deref()
            .map(|result| truncate_chars(result, REPORT_EXCERPT_MAX_CHARS))
            .unwrap_or_default();
        if excerpt.is_empty() {
            let _ = write!(report, "\n{}. [{}] {}", step.seq, step.status, step.title);
        } else {
            let _ = write!(
                report,
                "\n{}. [{}] {} — {}",
                step.seq, step.status, step.title, excerpt
            );
        }
    }
    report
}

/// Truncate text to `limit` characters (never splitting mid-scalar), marking
/// the cut. Empty input stays empty (no marker on untouched text).
pub(super) fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…[truncated]")
}
