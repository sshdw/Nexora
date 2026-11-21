//! In-run context compaction (Task T4): threshold-triggered and
//! overflow-driven folding of older turns into a summary.
//!
//! Long agent runs accumulate tool results until the prompt no longer fits
//! the model window. This module owns the pure policy: when to compact, how
//! much of the recent tail to retain verbatim, how to serialize the folded
//! head for the summarizer, and how to rebuild the history afterwards.
//!
//! The design re-implements the standard runtime-research semantics
//! (threshold trigger, retained-tail budget, cheap tool-output prune, and a
//! middle-out drop ladder for the summarizer call itself). Every line here
//! is written for Nexora's message model; no upstream source is vendored
//! and no upstream text is quoted.
//!
//! Rules the implementation follows:
//!
//! - Pure logic only: no DB, no HTTP, no threads, no clock (the style of
//!   [`super::pricing`]). The runner in [`super::runner`] performs the
//!   summarizer call and the event emission.
//! - Compaction rewrites the **in-run** history only. Nothing is persisted,
//!   and the summary travels as a normal user message carrying
//!   [`SUMMARY_PREFIX`] — Nexora has no visibility flags.
//! - `0` usable tokens disable the proactive trigger rather than compacting
//!   forever; unknown usage (`None`) never triggers.

use std::ops::Range;

use crate::application::execution::{AiMessage, AiRole, ExecutorError, TokenUsage};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Prompt fraction at which a proactive compaction is attempted.
pub(crate) const COMPACTION_THRESHOLD: f64 = 0.8;

/// Output headroom subtracted from the model context limit before comparing
/// usage: the window the prompt must fit, not the raw model limit.
pub(crate) const COMPACTION_RESERVED_TOKENS: u64 = 20_000;

/// Lower clamp for the retained-tail budget (tokens).
pub(crate) const MIN_PRESERVE_RECENT_TOKENS: u64 = 2_000;

/// Upper clamp for the retained-tail budget (tokens).
pub(crate) const MAX_PRESERVE_RECENT_TOKENS: u64 = 15_000;

/// Retained-tail budget as a fraction of the usable window.
pub(crate) const PRESERVE_RECENT_RATIO: f64 = 0.25;

/// Tool-result text clipped to this many characters **inside the summarizer
/// input only**; the stored in-run history is untouched by the clip.
pub(crate) const SUMMARY_TOOL_OUTPUT_MAX_CHARS: usize = 2_000;

/// Prune tier fires only when clearing tool outputs would free more than
/// this many estimated tokens.
pub(crate) const PRUNE_MINIMUM_TOKENS: u64 = 20_000;

/// Prune tier keeps this many estimated tokens of older tool output before
/// it starts clearing.
pub(crate) const PRUNE_PROTECT_TOKENS: u64 = 40_000;

/// Marker written over a pruned tool result. The call identity (`call_id`,
/// `name`) is preserved; only the bulky content is replaced.
pub(crate) const PRUNED_TOOL_OUTPUT_MARKER: &str = "[earlier tool output cleared to save context]";

/// Summarizer overflow ladder: percentage of tool-result messages dropped
/// from the summarizer input, middle-out, per attempt.
pub(crate) const REMOVAL_PERCENTAGES: [u8; 5] = [0, 10, 20, 50, 100];

/// Reactive recoveries allowed per run: after this many overflow-driven
/// compactions a further overflow is terminal.
pub(crate) const MAX_CONTEXT_ERROR_COMPACTIONS: usize = 2;

/// Cap on the summarizer's own output, enforced through the summary prompt
/// contract (the request carries no separate output control).
pub(crate) const SUMMARY_MAX_OUTPUT_TOKENS: u32 = 4_096;

/// Byte-based token estimate divisor: four characters per token.
pub(crate) const ESTIMATE_CHARS_PER_TOKEN: u64 = 4;

/// Safety margin on the estimate: the raw estimate is inflated by 20 %
/// (x6/5) so the retained tail is over-protected rather than measured.
pub(crate) const ESTIMATE_SAFETY_NUMERATOR: u64 = 6;
pub(crate) const ESTIMATE_SAFETY_DENOMINATOR: u64 = 5;

/// Marker prefixing the summary message. The summary is a normal user
/// message (no visibility flags exist), so the prefix is what tells the
/// model — and a reader — that this turn is folded history.
pub(crate) const SUMMARY_PREFIX: &str = "[Compaction summary — earlier turns of this run]";

/// Continuation prompt appended after the retained tail so the model resumes
/// the task instead of commenting on the compaction.
pub(crate) const CONTINUE_TEXT: &str = "Your context was compacted; the summary above plus the recent turns are your working state. \
     Do not mention the summary or that compaction happened. Continue the task where the recent turns left off.";

/// Structured summary contract for the summarizer call. `{history}` is
/// replaced with the serialized folded head, `{max_tokens}` with
/// [`SUMMARY_MAX_OUTPUT_TOKENS`]. Explicitly preserves Goal, Active Plan,
/// Key Decisions, and Touched Files / Open Tasks (M16).
pub(crate) const SUMMARY_TEMPLATE: &str = "You are compacting an autonomous coding agent's context. \
     Summarize the earlier turns below into a handoff note of at most {max_tokens} tokens, \
     written in plain text with exactly these sections in order (write \"none\" for any section with nothing to report):\n\
     \n\
     1. Goal — the user's original request and overall objective.\n\
     2. Active Plan — the active implementation plan, agreed steps, or \"none\".\n\
     3. Key Decisions — decisions, constraints, user approvals/denials, and discoveries the next turn must not forget.\n\
     4. Touched Files / Open Tasks — file paths read or modified with one line each, errors seen/fixed, completed progress, and remaining open tasks / immediate next action.\n\
     \n\
     Rules: preserve exact file paths, identifiers, and error text; drop tool-output noise; \
     never invent work that did not happen.\n\
     \n\
     --- earlier turns ---\n\
     {history}\n\
     --- end of earlier turns ---";

// ---------------------------------------------------------------------------
// Trigger math
// ---------------------------------------------------------------------------

/// Usable prompt window after reserving output headroom. A `0` context limit
/// (unknown model window) yields `0`, which disables compaction.
#[must_use]
pub(crate) fn usable_context_tokens(context_limit: u64) -> u64 {
    context_limit.saturating_sub(COMPACTION_RESERVED_TOKENS)
}

/// Proactive trigger: true once the provider-reported input tokens exceed
/// `threshold` of the usable window.
///
/// `None` usage (the provider reported none) never triggers, `threshold <=
/// 0.0 || threshold >= 1.0` disables auto-compaction, and a `0` usable
/// window disables it as well.
#[must_use]
#[allow(clippy::cast_precision_loss)] // windows far below 2^53 stay exact; above, the comparison only gets earlier
pub(crate) fn should_compact(
    usage: Option<TokenUsage>,
    context_limit: u64,
    threshold: f64,
) -> bool {
    if threshold <= 0.0 || threshold >= 1.0 {
        return false;
    }
    let usable = usable_context_tokens(context_limit);
    if usable == 0 {
        return false;
    }
    match usage {
        None => false,
        Some(report) => {
            // Exact for every realistic window (u64 -> f64 is lossless below
            // 2^53); the allow covers the abstract conversion, not a real error.
            (report.input_tokens as f64) / (usable as f64) > threshold
        }
    }
}

/// Retained-tail budget: `min(MAX, max(MIN, floor(usable * RATIO)))`.
#[must_use]
pub(crate) fn preserve_recent_budget(usable: u64) -> u64 {
    // Integer math for `floor(usable * PRESERVE_RECENT_RATIO)`: the ratio is
    // exactly one quarter, so this is `usable / 4` with no float involved.
    // The assert keeps the divisor honest if the ratio ever changes.
    debug_assert!((PRESERVE_RECENT_RATIO * 4.0 - 1.0).abs() < 1e-9);
    MAX_PRESERVE_RECENT_TOKENS.min(MIN_PRESERVE_RECENT_TOKENS.max(usable / 4))
}

/// Byte-based token estimate for `text`: characters divided by
/// [`ESTIMATE_CHARS_PER_TOKEN`], rounded up, then inflated by the safety
/// margin. Saturates instead of overflowing.
#[must_use]
pub(crate) fn estimate_tokens(text: &str) -> u64 {
    let chars = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
    let base = chars.div_ceil(ESTIMATE_CHARS_PER_TOKEN);
    base.saturating_mul(ESTIMATE_SAFETY_NUMERATOR) / ESTIMATE_SAFETY_DENOMINATOR
}

/// Usage ratio in thousandths (`ratio * 1000`), saturating at `u32::MAX`.
/// Carried on the threshold reason so events stay integer-typed.
#[must_use]
pub(crate) fn usage_ratio_milli(input_tokens: u64, usable: u64) -> u32 {
    if usable == 0 {
        return 0;
    }
    let scaled = u128::from(input_tokens).saturating_mul(1_000) / u128::from(usable);
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------
// Planning: exchange groups, tail selection, prune tier
// ---------------------------------------------------------------------------

/// Which messages are folded into the summary and which are retained.
///
/// `retain` always ends at the history end, always holds at least one
/// complete exchange, and never overlaps `summarize`. An empty `summarize`
/// range means there is nothing worth folding (single exchange, or
/// everything fits the budget): the caller must skip compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionPlan {
    /// `messages` range folded into the summary (never overlaps `retain`).
    pub(crate) summarize: Range<usize>,
    /// `messages` range re-sent verbatim (the retained tail).
    pub(crate) retain: Range<usize>,
    /// Estimated tokens of the retained tail.
    pub(crate) retained_tokens: u64,
}

/// Index where foldable history starts: past a leading system prompt, which
/// is preserved verbatim outside the plan.
fn fold_start(messages: &[AiMessage]) -> usize {
    usize::from(
        messages
            .first()
            .is_some_and(|first| first.role == AiRole::System),
    )
}

/// Split the foldable history into complete exchange groups: every
/// assistant or user message opens a group, tool results attach to the
/// running group. Groups are the atomic unit of retention — a group is
/// never split, so tool calls and their results stay together.
fn exchange_groups(messages: &[AiMessage], start: usize) -> Vec<Range<usize>> {
    let mut groups: Vec<Range<usize>> = Vec::new();
    let mut group_start = start;
    for (index, message) in messages.iter().enumerate().skip(start) {
        if index > group_start
            && (message.role == AiRole::Assistant || message.role == AiRole::User)
        {
            groups.push(group_start..index);
            group_start = index;
        }
    }
    if group_start < messages.len() {
        groups.push(group_start..messages.len());
    }
    groups
}

/// Estimated tokens of one message for budgeting: visible text, structured
/// tool calls (names plus arguments), and the tool result.
pub(crate) fn message_tokens(message: &AiMessage) -> u64 {
    let mut total = estimate_tokens(&message.composed_content());
    for call in &message.tool_calls {
        total = total.saturating_add(estimate_tokens(&call.name));
        total = total.saturating_add(estimate_tokens(&call.arguments));
    }
    if let Some(result) = &message.tool_result {
        total = total.saturating_add(estimate_tokens(&result.name));
        total = total.saturating_add(estimate_tokens(&result.content));
    }
    total
}

/// Tail selection: walk backwards in complete exchange groups while the
/// accumulated estimate fits `budget`. Always retains at least one complete
/// exchange and never summarizes everything: with a single group (or when
/// everything fits) the summarize range is empty and the whole history is
/// retained.
#[must_use]
pub(crate) fn plan_compaction(messages: &[AiMessage], budget: u64) -> CompactionPlan {
    let start = fold_start(messages);
    if messages.len() <= start.saturating_add(1) {
        return CompactionPlan {
            summarize: start..start,
            retain: start..messages.len(),
            retained_tokens: messages[start..].iter().map(message_tokens).sum(),
        };
    }
    let groups = exchange_groups(messages, start);
    if groups.is_empty() {
        return CompactionPlan {
            summarize: start..start,
            retain: start..messages.len(),
            retained_tokens: 0,
        };
    }
    let estimates: Vec<u64> = groups
        .iter()
        .map(|range| messages[range.clone()].iter().map(message_tokens).sum())
        .collect();
    // The newest exchange is always retained, even over budget.
    let mut keep = groups.len().saturating_sub(1);
    let mut total = estimates[keep];
    for (index, estimate) in estimates.iter().enumerate().take(keep).rev() {
        if total.saturating_add(*estimate) <= budget {
            total = total.saturating_add(*estimate);
            keep = index;
        } else {
            break;
        }
    }
    if keep == 0 {
        // Every group fits (or there is only one): retain everything,
        // summarize nothing.
        let retained_tokens = estimates
            .iter()
            .fold(0_u64, |sum, estimate| sum.saturating_add(*estimate));
        CompactionPlan {
            summarize: start..start,
            retain: start..messages.len(),
            retained_tokens,
        }
    } else {
        CompactionPlan {
            summarize: start..groups[keep].start,
            retain: groups[keep].start..messages.len(),
            retained_tokens: total,
        }
    }
}

/// Cheap prune tier: clear tool-result contents older than the protected
/// budget, never touching the newest two exchange groups and never touching
/// `protected` (the retained tail). Fires only when the freed total exceeds
/// [`PRUNE_MINIMUM_TOKENS`]; otherwise nothing is mutated and `0` returns.
///
/// Only tool results are ever cleared — assistant narration and the user
/// request are untouched — and call identity is preserved under the marker.
pub(crate) fn prune_tool_outputs(messages: &mut [AiMessage], protected: &Range<usize>) -> u64 {
    let start = fold_start(messages);
    let groups = exchange_groups(messages, start);
    let newest_two_start = groups
        .iter()
        .rev()
        .take(2)
        .map(|range| range.start)
        .min()
        .unwrap_or(messages.len());
    let mut candidates: Vec<(usize, u64)> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if message.role != AiRole::Tool || message.tool_result.is_none() {
            continue;
        }
        if index >= newest_two_start || protected.contains(&index) {
            continue;
        }
        candidates.push((index, message_tokens(message)));
    }
    // Oldest-first accumulation: contents younger than the protected budget
    // of tool output survive; older ones are marked for clearing.
    let mut running: u64 = 0;
    let mut marked: Vec<(usize, u64)> = Vec::new();
    for (index, estimate) in candidates {
        running = running.saturating_add(estimate);
        if running > PRUNE_PROTECT_TOKENS {
            marked.push((index, estimate));
        }
    }
    let freed = marked
        .iter()
        .fold(0_u64, |sum, (_, estimate)| sum.saturating_add(*estimate));
    if freed <= PRUNE_MINIMUM_TOKENS {
        return 0;
    }
    for (index, _) in marked {
        if let Some(result) = messages[index].tool_result.as_mut() {
            result.content = PRUNED_TOOL_OUTPUT_MARKER.to_string();
        }
    }
    freed
}

// ---------------------------------------------------------------------------
// Summarizer input
// ---------------------------------------------------------------------------

/// Clip `text` to at most `max` characters on a char boundary.
fn clip_chars(text: &str, max: usize) -> &str {
    if text.chars().count() <= max {
        return text;
    }
    let end = text
        .char_indices()
        .take(max)
        .last()
        .map_or(0, |(index, _)| index);
    &text[..end]
}

/// Serialize one message for the summarizer input. Tool results are clipped
/// to [`SUMMARY_TOOL_OUTPUT_MAX_CHARS`] here only; the stored history keeps
/// the full text.
#[must_use]
pub(crate) fn serialize_for_summary(message: &AiMessage) -> String {
    match message.role {
        AiRole::System => format!("System: {}", message.composed_content()),
        AiRole::User => format!("User: {}", message.composed_content()),
        AiRole::Assistant => {
            use std::fmt::Write as _;
            let mut out = format!("Assistant: {}", message.content);
            for call in &message.tool_calls {
                let _ = write!(
                    out,
                    "\nAssistant tool call {} ({}): {}",
                    call.id, call.name, call.arguments
                );
            }
            out
        }
        AiRole::Tool => {
            let (call_id, name, content) = match &message.tool_result {
                Some(result) => (
                    result.call_id.as_str(),
                    result.name.as_str(),
                    result.content.as_str(),
                ),
                None => ("?", "?", ""),
            };
            format!(
                "Tool result for {call_id} ({name}): {}",
                clip_chars(content, SUMMARY_TOOL_OUTPUT_MAX_CHARS)
            )
        }
    }
}

/// Build the summarizer prompt for `plan` over `messages`: the structured
/// contract with the serialized folded head filled in.
#[must_use]
pub(crate) fn summary_prompt(messages: &[AiMessage], plan: &CompactionPlan) -> String {
    summary_prompt_for_slice(&messages[plan.summarize.clone()])
}

/// Build the summarizer prompt over an already-selected head slice (the
/// overflow ladder filters the head before calling this).
#[must_use]
pub(crate) fn summary_prompt_for_slice(head: &[AiMessage]) -> String {
    let history = head
        .iter()
        .map(serialize_for_summary)
        .collect::<Vec<_>>()
        .join("\n\n");
    SUMMARY_TEMPLATE
        .replace("{history}", &history)
        .replace("{max_tokens}", &SUMMARY_MAX_OUTPUT_TOKENS.to_string())
}

/// Keep the summarizer input fittable: drop `percent` of the tool-result
/// messages, middle-out (oldest and newest context survive longest), keeping
/// every survivor in order. `percent == 0` — or no tool results at all —
/// returns the history unchanged. `percent >= 100` clears every tool result:
/// the final ladder rung must leave nothing behind for the failure message
/// ("even after removing all tool responses") to stay honest.
/// Contiguous middle-out window over `len` tool results: exactly `count`
/// positions centered on the middle (`len / 2`), left-biased on ties so a
/// single drop takes the just-left-of-center entry. Clamped so the window
/// always holds `min(count, len)` positions.
#[must_use]
pub(crate) fn middle_out_window(len: usize, count: usize) -> Range<usize> {
    let count = count.min(len);
    let middle = len / 2;
    let start = middle
        .saturating_sub(count.div_ceil(2))
        .min(len.saturating_sub(count));
    start..start.saturating_add(count)
}

/// Keep the summarizer input fittable: drop `percent` of the tool-result
/// messages, middle-out (oldest and newest context survive longest), keeping
/// every survivor in order. `percent == 0` — or no tool results at all —
/// returns the history unchanged. `percent >= 100` clears every tool result:
/// the final ladder rung must leave nothing behind for the failure message
/// ("even after removing all tool responses") to stay honest.
#[must_use]
pub(crate) fn filter_tool_responses(messages: &[AiMessage], percent: u8) -> Vec<AiMessage> {
    if percent == 0 {
        return messages.to_vec();
    }
    if percent >= 100 {
        return messages
            .iter()
            .filter(|message| !(message.role == AiRole::Tool && message.tool_result.is_some()))
            .cloned()
            .collect();
    }
    let tool_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == AiRole::Tool && message.tool_result.is_some())
        .map(|(index, _)| index)
        .collect();
    if tool_indices.is_empty() {
        return messages.to_vec();
    }
    let num_to_remove = ((tool_indices.len() * usize::from(percent)) / 100)
        .max(1)
        .min(tool_indices.len());
    let mut drop = vec![false; messages.len()];
    for position in middle_out_window(tool_indices.len(), num_to_remove) {
        drop[tool_indices[position]] = true;
    }
    messages
        .iter()
        .enumerate()
        .filter(|(index, _)| !drop[*index])
        .map(|(_, message)| message.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Rebuild
// ---------------------------------------------------------------------------

/// A plain user message carrying `content` (summary and continuation turns).
fn user_message(content: String) -> AiMessage {
    AiMessage {
        role: AiRole::User,
        content,
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_result: None,
    }
}

/// Rebuild `messages` in post-compaction order: `[System, User(summary),
/// ..retained tail.., User(continue)]`, then merge adjacent same-role turns
/// that carry no tool structure (tool calls and their results never merge,
/// so call/result pairing survives verbatim).
pub(crate) fn apply_compaction(
    messages: &mut Vec<AiMessage>,
    summary: &str,
    plan: &CompactionPlan,
) {
    let has_system = messages
        .first()
        .is_some_and(|first| first.role == AiRole::System);
    let retained = messages[plan.retain.clone()].to_vec();
    let mut rebuilt: Vec<AiMessage> = Vec::with_capacity(retained.len() + 3);
    if has_system {
        rebuilt.push(messages[0].clone());
    }
    rebuilt.push(user_message(format!("{SUMMARY_PREFIX}\n\n{summary}")));
    rebuilt.extend(retained);
    rebuilt.push(user_message(CONTINUE_TEXT.to_string()));
    let mut merged: Vec<AiMessage> = Vec::with_capacity(rebuilt.len());
    for message in rebuilt {
        let mergeable = message.tool_calls.is_empty() && message.tool_result.is_none();
        if mergeable {
            if let Some(previous) = merged.last_mut() {
                if previous.role == message.role
                    && previous.tool_calls.is_empty()
                    && previous.tool_result.is_none()
                {
                    previous.content.push_str("\n\n");
                    previous.content.push_str(&message.content);
                    previous.attachments.extend(message.attachments);
                    continue;
                }
            }
        }
        merged.push(message);
    }
    *messages = merged;
}

// ---------------------------------------------------------------------------
// Run-scoped governor (owned by the loop, never persisted)
// ---------------------------------------------------------------------------

/// What the loop must do at the next step boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextAction {
    /// Send the next request as-is.
    Proceed,
    /// Compact first, then send.
    Compact(CompactionReason),
    /// No further progress is possible: terminate the run.
    Exhausted,
}

/// Structured compaction fields explicitly preserved across auto and manual
/// compaction (M16: Goal, Active Plan, Key Decisions, Touched Files / Open Tasks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructuredCompactionSummary {
    pub goal: String,
    pub active_plan: String,
    pub key_decisions: String,
    pub touched_files: Vec<String>,
    pub open_tasks: String,
    pub formatted: String,
}

fn push_unique_path(paths: &mut Vec<String>, candidate: &str) {
    let trimmed = candidate.trim();
    if trimmed.is_empty() || trimmed.len() > 260 {
        return;
    }
    if !paths.iter().any(|existing| existing == trimmed) {
        paths.push(trimmed.to_string());
    }
}

fn extract_path_from_json_args(arguments: &str) -> Option<String> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return None;
    };
    for key in ["path", "file_path", "target", "dir_path", "file"] {
        if let Some(s) = val.get(key).and_then(serde_json::Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn clip_summary_line(text: &str, max_chars: usize) -> String {
    let single = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = single.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let kept: String = trimmed.chars().take(max_chars).collect();
    format!("{kept}…")
}

fn collect_decision_lines(text: &str, decisions: &mut Vec<String>) {
    for line in text.lines() {
        let l = line.trim();
        let lower = l.to_ascii_lowercase();
        if (lower.contains("decid")
            || lower.contains("chosen")
            || lower.contains("approved")
            || lower.contains("constraint")
            || lower.contains("must ")
            || lower.contains("will use"))
            && decisions.len() < 6
        {
            decisions.push(clip_summary_line(l, 200));
        }
    }
}

/// Deterministically extract **Goal**, **Active Plan**, **Key Decisions**, and
/// **Touched Files / Open Tasks** from a message slice and optional plan/files.
#[must_use]
pub(crate) fn extract_structured_summary(
    head: &[AiMessage],
    active_plan: Option<&str>,
    extra_touched_files: &[String],
) -> StructuredCompactionSummary {
    let mut goal = String::new();
    let mut decisions: Vec<String> = Vec::new();
    let mut touched_files: Vec<String> = Vec::new();
    let mut open_tasks: Vec<String> = Vec::new();

    for path in extra_touched_files {
        push_unique_path(&mut touched_files, path);
    }

    for msg in head {
        match msg.role {
            AiRole::User => {
                let text = msg.composed_content();
                let trimmed = text.trim();
                if trimmed.is_empty()
                    || trimmed == CONTINUE_TEXT
                    || trimmed.starts_with(SUMMARY_PREFIX)
                {
                    continue;
                }
                if goal.is_empty() {
                    goal = clip_summary_line(trimmed, 400);
                } else {
                    open_tasks.push(clip_summary_line(trimmed, 220));
                }
            }
            AiRole::Assistant => {
                for call in &msg.tool_calls {
                    if let Some(path) = extract_path_from_json_args(&call.arguments) {
                        push_unique_path(&mut touched_files, &path);
                    }
                }
                if !msg.content.trim().is_empty() {
                    collect_decision_lines(msg.content.trim(), &mut decisions);
                }
            }
            AiRole::Tool => {
                if let Some(res) = &msg.tool_result {
                    if res.content.contains("denied") {
                        decisions.push(format!(
                            "Tool '{}' denied: {}",
                            res.name,
                            clip_summary_line(&res.content, 160)
                        ));
                    }
                }
            }
            AiRole::System => {}
        }
    }

    if goal.is_empty() {
        goal = "none".to_string();
    }
    let active_plan_text = active_plan
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| "none".to_string(), |s| clip_summary_line(s, 600));
    let key_decisions = if decisions.is_empty() {
        "none".to_string()
    } else {
        decisions.join("; ")
    };
    let open_tasks_text = if open_tasks.is_empty() {
        "none".to_string()
    } else {
        open_tasks
            .into_iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("; ")
    };

    let files_part = if touched_files.is_empty() {
        "none".to_string()
    } else {
        touched_files.join(", ")
    };
    let formatted = format!(
        "1. Goal\n{goal}\n\n2. Active Plan\n{active_plan_text}\n\n3. Key Decisions\n{key_decisions}\n\n4. Touched Files / Open Tasks\nFiles: {files_part}\nOpen tasks: {open_tasks_text}"
    );

    StructuredCompactionSummary {
        goal,
        active_plan: active_plan_text,
        key_decisions,
        touched_files,
        open_tasks: open_tasks_text,
        formatted,
    }
}

/// Combine an LLM-generated summary with deterministic preserved sections so
/// **Goal**, **Active Plan**, **Key Decisions**, and **Touched Files / Open Tasks**
/// are never lost even if the model omits a section.
#[must_use]
pub(crate) fn merge_summary_with_preserved_sections(
    llm_summary: Option<&str>,
    structured: &StructuredCompactionSummary,
) -> String {
    let Some(raw) = llm_summary.map(str::trim).filter(|s| !s.is_empty()) else {
        return structured.formatted.clone();
    };
    let lower = raw.to_ascii_lowercase();
    let mut out = raw.to_string();
    if !lower.contains("goal") && structured.goal != "none" {
        out.push_str("\n\n1. Goal\n");
        out.push_str(&structured.goal);
    }
    if !lower.contains("active plan") && structured.active_plan != "none" {
        out.push_str("\n\n2. Active Plan\n");
        out.push_str(&structured.active_plan);
    }
    if !lower.contains("key decision") && structured.key_decisions != "none" {
        out.push_str("\n\n3. Key Decisions\n");
        out.push_str(&structured.key_decisions);
    }
    let missing_files: Vec<&str> = structured
        .touched_files
        .iter()
        .map(String::as_str)
        .filter(|path| !raw.contains(*path))
        .collect();
    if !missing_files.is_empty() {
        out.push_str("\n\n4. Touched Files / Open Tasks\nFiles: ");
        out.push_str(&missing_files.join(", "));
    }
    out
}

/// Why a compaction was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionReason {
    /// Provider-reported usage crossed [`COMPACTION_THRESHOLD`].
    Threshold {
        /// Usage ratio in thousandths (see [`usage_ratio_milli`]).
        ratio_milli: u32,
    },
    /// A provider reported a context-length failure.
    Overflow,
    /// Explicit user-initiated compaction (`/compact` or `ContextRing` button).
    Manual,
}

impl CompactionReason {
    /// Stable event string for this reason (`"threshold"` / `"overflow"` / `"manual"`).
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Threshold { .. } => "threshold",
            Self::Overflow => "overflow",
            Self::Manual => "manual",
        }
    }
}

/// Run-scoped compaction state, owned by the loop and never persisted.
/// Tracks the previous turn's usage plus the per-run recovery caps.
#[derive(Debug, Default)]
pub(crate) struct ContextGovernor {
    /// Provider-reported usage of the previous turn (`None` until observed).
    last_usage: Option<TokenUsage>,
    /// Proactive compactions completed in this run.
    compactions: usize,
    /// Reactive (overflow) recoveries used in this run.
    context_errors: usize,
    /// Set when no further progress is possible.
    exhausted: bool,
}

impl ContextGovernor {
    /// A fresh governor: no usage observed, no recoveries used.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record the usage of the turn that just finished.
    pub(crate) fn observe(&mut self, usage: Option<TokenUsage>) {
        self.last_usage = usage;
    }

    /// Decide at the step boundary *before* the next request — the only
    /// place proactive compaction may run. Histories too short to fold and
    /// unknown model windows always proceed.
    #[must_use]
    pub(crate) fn decide(&self, messages: &[AiMessage], context_limit: u64) -> ContextAction {
        if self.exhausted {
            return ContextAction::Exhausted;
        }
        if messages.len() <= 2 {
            return ContextAction::Proceed;
        }
        let usable = usable_context_tokens(context_limit);
        if usable == 0 {
            return ContextAction::Proceed;
        }
        match self.last_usage {
            Some(usage) if should_compact(Some(usage), context_limit, COMPACTION_THRESHOLD) => {
                ContextAction::Compact(CompactionReason::Threshold {
                    ratio_milli: usage_ratio_milli(usage.input_tokens, usable),
                })
            }
            None | Some(_) => ContextAction::Proceed,
        }
    }

    /// Map a classified provider failure onto a reactive recovery request.
    /// Returns true only for context-length failures while the per-run cap
    /// still allows a recovery; a failure past the cap marks the governor
    /// exhausted so the next boundary terminates instead of looping.
    pub(crate) fn note_provider_error(&mut self, error: &ExecutorError) -> bool {
        if !matches!(error, ExecutorError::ContextLengthExceeded) {
            return false;
        }
        if self.context_errors >= MAX_CONTEXT_ERROR_COMPACTIONS {
            self.exhausted = true;
            return false;
        }
        self.context_errors += 1;
        true
    }

    /// Record a completed compaction so the run's counts advance.
    pub(crate) fn note_compaction(&mut self, reason: CompactionReason) {
        self.compactions += 1;
        if reason == CompactionReason::Overflow {
            // Reactive recoveries are counted in `note_provider_error`; the
            // total only tracks that a compaction completed.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input_tokens: u64) -> TokenUsage {
        TokenUsage {
            input_tokens,
            output_tokens: 0,
            cached_tokens: 0,
        }
    }

    fn user(content: &str) -> AiMessage {
        AiMessage {
            role: AiRole::User,
            content: content.to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }
    }

    fn assistant_with_tool(id: &str, name: &str, output: &str) -> (AiMessage, AiMessage) {
        use crate::application::execution::{AiToolResult, ToolCall};
        (
            AiMessage {
                role: AiRole::Assistant,
                content: String::new(),
                attachments: Vec::new(),
                tool_calls: vec![ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    arguments: "{}".to_string(),
                    thought_signature: None,
                }],
                tool_result: None,
            },
            AiMessage {
                role: AiRole::Tool,
                content: String::new(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: Some(AiToolResult {
                    call_id: id.to_string(),
                    name: name.to_string(),
                    content: output.to_string(),
                }),
            },
        )
    }

    fn system() -> AiMessage {
        AiMessage {
            role: AiRole::System,
            content: "sys".to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }
    }

    #[test]
    fn trigger_math_threshold_none_and_disabled() {
        let limit = COMPACTION_RESERVED_TOKENS + 100_000;
        let usable = usable_context_tokens(limit);
        assert_eq!(usable, 100_000);
        // At 0.8 exactly the trigger stays off; above it fires.
        assert!(!should_compact(
            Some(usage(80_000)),
            limit,
            COMPACTION_THRESHOLD
        ));
        assert!(should_compact(
            Some(usage(80_001)),
            limit,
            COMPACTION_THRESHOLD
        ));
        // Unknown usage never triggers.
        assert!(!should_compact(None, limit, COMPACTION_THRESHOLD));
        // Degenerate thresholds disable auto-compaction.
        assert!(!should_compact(Some(usage(200_000)), limit, 0.0));
        assert!(!should_compact(Some(usage(200_000)), limit, 1.0));
        assert!(!should_compact(Some(usage(200_000)), limit, 2.0));
        // Unknown model window disables the trigger.
        assert_eq!(usable_context_tokens(0), 0);
        assert!(!should_compact(
            Some(usage(u64::MAX)),
            0,
            COMPACTION_THRESHOLD
        ));
        assert!(!should_compact(
            Some(usage(u64::MAX)),
            COMPACTION_RESERVED_TOKENS,
            COMPACTION_THRESHOLD
        ));
    }

    #[test]
    fn budget_clamps_to_min_max_and_ratio() {
        assert_eq!(preserve_recent_budget(0), MIN_PRESERVE_RECENT_TOKENS);
        assert_eq!(preserve_recent_budget(1_000), MIN_PRESERVE_RECENT_TOKENS);
        // 2000 * 4 = 8000 usable hits the floor exactly.
        assert_eq!(preserve_recent_budget(8_000), MIN_PRESERVE_RECENT_TOKENS);
        assert_eq!(preserve_recent_budget(40_000), 10_000);
        assert_eq!(preserve_recent_budget(100_000), MAX_PRESERVE_RECENT_TOKENS);
        assert_eq!(preserve_recent_budget(u64::MAX), MAX_PRESERVE_RECENT_TOKENS);
    }

    #[test]
    fn estimate_chars_per_token_with_safety() {
        assert_eq!(estimate_tokens(""), 0);
        // 4 chars -> 1 token, x1.2 -> 1 (integer floor of 6/5).
        assert_eq!(estimate_tokens("abcd"), 1);
        // 20 chars -> 5 tokens, x1.2 -> 6.
        assert_eq!(estimate_tokens(&"x".repeat(20)), 6);
        assert_eq!(estimate_tokens(&"x".repeat(21)), 7);
    }

    #[test]
    fn plan_never_summarizes_everything() {
        // Single exchange over a tiny budget: the exchange itself is always
        // retained; only older turns (here: the user request) fold.
        let (call, result) = assistant_with_tool("1", "read_file", "output one");
        let messages = vec![system(), user("do it"), call, result];
        let plan = plan_compaction(&messages, 1);
        assert_eq!(plan.retain, 2..messages.len());
        assert_eq!(plan.summarize, 1..2);
        // Same history with room for everything: retain all, summarize nothing.
        let plan = plan_compaction(&messages, u64::MAX);
        assert!(plan.summarize.is_empty());
        assert_eq!(plan.retain, 1..messages.len());
        // Minimal history: nothing to fold.
        let tiny = vec![system(), user("hi")];
        let plan = plan_compaction(&tiny, 1);
        assert!(plan.summarize.is_empty());
        assert_eq!(plan.retain, 1..tiny.len());
    }

    #[test]
    fn plan_folds_head_and_keeps_budgeted_tail() {
        let mut messages = vec![system(), user("request")];
        for turn in 0..4 {
            let (call, result) =
                assistant_with_tool(&format!("c{turn}"), "read_file", &"y".repeat(2_000));
            messages.push(call);
            messages.push(result);
        }
        // A budget holding roughly one exchange must keep the newest group
        // and fold everything older — but never the whole history.
        let plan = plan_compaction(&messages, 1_500);
        assert!(!plan.retain.is_empty());
        assert_eq!(plan.retain.end, messages.len());
        assert!(!plan.summarize.is_empty());
        assert_eq!(plan.summarize.end, plan.retain.start);
        assert!(plan.retain.start >= 1);
        // The newest tool output survives in the retained tail.
        let tail_text: String = messages[plan.retain.clone()]
            .iter()
            .map(serialize_for_summary)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tail_text.contains(&"y".repeat(100)));
        // A huge budget retains everything and summarizes nothing.
        let plan = plan_compaction(&messages, u64::MAX);
        assert!(plan.summarize.is_empty());
        assert_eq!(plan.retain, 1..messages.len());
    }

    #[test]
    fn prune_protects_newest_two_exchanges() {
        // Six large exchanges: the prune budget (40k tokens) keeps the
        // oldest tool output plus the newest two exchanges, clearing the
        // middle three for ~90k freed tokens — far above the 20k minimum.
        let mut messages = vec![system(), user("request")];
        for turn in 0..6 {
            let (call, result) =
                assistant_with_tool(&format!("c{turn}"), "read_file", &"z".repeat(100_000));
            messages.push(call);
            messages.push(result);
        }
        let retain = messages.len()..messages.len();
        let freed = prune_tool_outputs(&mut messages, &retain);
        assert!(
            freed > PRUNE_MINIMUM_TOKENS,
            "old tool output must free above the minimum, freed {freed}"
        );
        // The newest two exchanges (4 tool messages) keep full content.
        for message in &messages[messages.len() - 4..] {
            if let Some(result) = &message.tool_result {
                assert!(
                    !result.content.contains(PRUNED_TOOL_OUTPUT_MARKER),
                    "newest two exchanges must survive the prune"
                );
            }
        }
        // The oldest tool result sits inside the protected budget: intact.
        let oldest = messages
            .iter()
            .find_map(|message| message.tool_result.as_ref())
            .expect("oldest tool result");
        assert!(!oldest.content.contains(PRUNED_TOOL_OUTPUT_MARKER));
        // The middle outputs were cleared.
        let cleared = messages
            .iter()
            .filter_map(|message| message.tool_result.as_ref())
            .filter(|result| result.content == PRUNED_TOOL_OUTPUT_MARKER)
            .count();
        assert_eq!(cleared, 3, "exactly the middle three outputs clear");
    }

    #[test]
    fn prune_below_minimum_mutates_nothing() {
        let (call_a, result_a) = assistant_with_tool("1", "read_file", "small");
        let (call_b, result_b) = assistant_with_tool("2", "read_file", "small");
        let mut messages = vec![
            system(),
            user("request"),
            call_a,
            result_a,
            call_b,
            result_b,
        ];
        let retain = messages.len()..messages.len();
        assert_eq!(prune_tool_outputs(&mut messages, &retain), 0);
        assert!(messages
            .iter()
            .filter_map(|message| message.tool_result.as_ref())
            .all(|outcome| !outcome.content.contains(PRUNED_TOOL_OUTPUT_MARKER)));
    }

    #[test]
    fn middle_out_ladder_drops_center_first() {
        let mut messages = vec![system(), user("request")];
        for turn in 0..5 {
            let (call, result) =
                assistant_with_tool(&format!("c{turn}"), "read_file", &format!("out{turn}"));
            messages.push(call);
            messages.push(result);
        }
        // 0 % keeps everything; the surviving order never changes.
        let kept = filter_tool_responses(&messages, 0);
        assert_eq!(kept, messages);
        // 20 % of 5 tool results drops exactly one, middle-out: with five
        // survivors the alternating offsets take the just-left-of-center
        // one first (out1), then out2, and so on.
        let kept = filter_tool_responses(&messages, 20);
        let texts: Vec<&str> = kept
            .iter()
            .filter_map(|message| message.tool_result.as_ref())
            .map(|result| result.content.as_str())
            .collect();
        assert_eq!(texts, vec!["out0", "out2", "out3", "out4"]);
        // 100 % drops every tool result but keeps the turns themselves.
        let kept = filter_tool_responses(&messages, 100);
        assert!(kept
            .iter()
            .find_map(|message| message.tool_result.as_ref())
            .is_none());
        assert_eq!(kept.len(), messages.len() - 5);
        // Histories without tool results pass through untouched.
        let plain = vec![system(), user("hi")];
        assert_eq!(filter_tool_responses(&plain, 100), plain);
    }

    #[test]
    fn single_tool_result_drops_at_partial_rung() {
        let (call, result) = assistant_with_tool("1", "read_file", "only");
        let messages = vec![system(), user("request"), call, result];
        // floor(1 * percent / 100) is 0, so the min-1 rule must still drop
        // the lone tool result at every partial rung.
        for percent in [10, 20, 50] {
            let kept = filter_tool_responses(&messages, percent);
            assert!(
                kept.iter()
                    .find_map(|message| message.tool_result.as_ref())
                    .is_none(),
                "N=1 at {percent}% must drop the single tool result"
            );
            assert_eq!(kept.len(), messages.len() - 1);
        }
    }

    #[test]
    fn middle_out_window_covers_all_when_count_equals_len() {
        // The loop-path index selection must be able to cover every tool
        // result, so a full-count window leaves no survivor behind.
        assert_eq!(middle_out_window(3, 3), 0..3);
        assert_eq!(middle_out_window(1, 1), 0..1);
        assert_eq!(middle_out_window(5, 1), 1..2);
        // Accepted left-bias: (5,3) skews left to preserve the (5,1)->1..2 contract.
        assert_eq!(middle_out_window(5, 3), 0..3);
        assert_eq!(middle_out_window(6, 3), 1..4);
        assert_eq!(middle_out_window(5, 2), 1..3);
        assert_eq!(middle_out_window(4, 2), 1..3);
    }

    #[test]
    fn governor_caps_reactive_recovery_and_exhausts() {
        let mut governor = ContextGovernor::new();
        let overflow = ExecutorError::ContextLengthExceeded;
        assert!(governor.note_provider_error(&overflow));
        assert!(governor.note_provider_error(&overflow));
        assert_eq!(governor.context_errors, 2);
        // The third overflow is refused and marks the governor exhausted.
        assert!(!governor.note_provider_error(&overflow));
        assert!(governor.exhausted);
        // Non-context failures never count and never exhaust.
        let mut other = ContextGovernor::new();
        assert!(!other.note_provider_error(&ExecutorError::Failure));
        assert_eq!(other.context_errors, 0);
        assert!(!other.exhausted);
    }

    #[test]
    fn governor_exhausted_path_terminates() {
        let mut governor = ContextGovernor::new();
        let overflow = ExecutorError::ContextLengthExceeded;
        let messages = vec![system(), user("request")];
        assert_eq!(governor.decide(&messages, 100_000), ContextAction::Proceed);
        governor.note_provider_error(&overflow);
        governor.note_provider_error(&overflow);
        assert!(!governor.note_provider_error(&overflow));
        assert_eq!(
            governor.decide(&messages, 100_000),
            ContextAction::Exhausted
        );
        governor.note_compaction(CompactionReason::Overflow);
        assert_eq!(governor.compactions, 1);
    }

    #[test]
    fn governor_proactive_decision_needs_usage_and_history() {
        let mut governor = ContextGovernor::new();
        let (call, result) = assistant_with_tool("1", "read_file", "out");
        let messages = vec![system(), user("request"), call, result];
        let limit = COMPACTION_RESERVED_TOKENS + 10_000;
        // No usage observed yet: proceed.
        assert_eq!(governor.decide(&messages, limit), ContextAction::Proceed);
        // Usage under the threshold: proceed.
        governor.observe(Some(usage(5_000)));
        assert_eq!(governor.decide(&messages, limit), ContextAction::Proceed);
        // Usage over the threshold: compact with the measured ratio.
        governor.observe(Some(usage(9_000)));
        assert_eq!(
            governor.decide(&messages, limit),
            ContextAction::Compact(CompactionReason::Threshold { ratio_milli: 900 })
        );
        governor.note_compaction(CompactionReason::Threshold { ratio_milli: 900 });
        assert_eq!(governor.compactions, 1);
        // Histories too short to fold always proceed.
        let tiny = vec![system(), user("hi")];
        assert_eq!(governor.decide(&tiny, limit), ContextAction::Proceed);
    }

    #[test]
    fn governor_decide_pins_threshold_boundary_and_reserved_window() {
        // Reserved-headroom math is pinned: the usable window is the raw
        // limit minus the 20k output reservation, saturating at zero.
        assert_eq!(COMPACTION_RESERVED_TOKENS, 20_000);
        assert_eq!(
            usable_context_tokens(COMPACTION_RESERVED_TOKENS + 100_000),
            100_000
        );
        assert_eq!(usable_context_tokens(COMPACTION_RESERVED_TOKENS), 0);
        assert_eq!(usable_context_tokens(0), 0);
        assert_eq!(
            usable_context_tokens(COMPACTION_RESERVED_TOKENS - 1),
            0,
            "a limit below the reservation saturates instead of wrapping"
        );

        // At exactly the 0.8 ratio the governor stays on Proceed; one token
        // above it compacts with the measured threshold reason.
        let limit = COMPACTION_RESERVED_TOKENS + 100_000;
        let (call, result) = assistant_with_tool("1", "read_file", "out");
        let messages = vec![system(), user("request"), call, result];
        let mut governor = ContextGovernor::new();
        governor.observe(Some(usage(80_000)));
        assert_eq!(
            governor.decide(&messages, limit),
            ContextAction::Proceed,
            "exactly 0.8 stays Proceed (strict `>` trigger)"
        );
        governor.observe(Some(usage(80_001)));
        assert_eq!(
            governor.decide(&messages, limit),
            ContextAction::Compact(CompactionReason::Threshold { ratio_milli: 800 }),
            "just above 0.8 compacts with the measured ratio"
        );
        // Dormant at zero: no usable window never compacts, however large
        // the reported usage is.
        governor.observe(Some(usage(u64::MAX)));
        assert_eq!(
            governor.decide(&messages, 0),
            ContextAction::Proceed,
            "unknown model window keeps the trigger dormant"
        );
    }

    #[test]
    fn summary_serialization_clips_tool_output_only() {
        let long = "w".repeat(SUMMARY_TOOL_OUTPUT_MAX_CHARS + 500);
        let (_, result) = assistant_with_tool("7", "read_file", &long);
        let serialized = serialize_for_summary(&result);
        assert!(serialized.contains("Tool result for 7 (read_file)"));
        assert!(!serialized.contains(&"w".repeat(SUMMARY_TOOL_OUTPUT_MAX_CHARS + 1)));
        // The stored history keeps the full text: only the summarizer input clips.
        assert_eq!(
            result
                .tool_result
                .as_ref()
                .expect("tool result")
                .content
                .len(),
            SUMMARY_TOOL_OUTPUT_MAX_CHARS + 500
        );
    }

    #[test]
    fn apply_compaction_orders_and_merges() {
        let (call, result) = assistant_with_tool("1", "read_file", "old output");
        let mut messages = vec![system(), user("request"), call, result];
        let plan = CompactionPlan {
            summarize: 1..3,
            retain: 3..messages.len(),
            retained_tokens: 10,
        };
        apply_compaction(&mut messages, "did stuff", &plan);
        assert_eq!(messages[0].role, AiRole::System);
        assert_eq!(messages[1].role, AiRole::User);
        assert!(messages[1].content.starts_with(SUMMARY_PREFIX));
        assert!(messages[1].content.contains("did stuff"));
        assert_eq!(messages.last().expect("continue").role, AiRole::User);
        assert!(messages
            .last()
            .expect("continue")
            .content
            .contains("compacted"));
        // Tool pairing in the retained tail survives verbatim.
        assert!(messages
            .iter()
            .filter_map(|message| message.tool_result.as_ref())
            .any(|outcome| outcome.content == "old output"));
    }

    #[test]
    fn structured_summary_preserves_goal_active_plan_decisions_and_touched_files() {
        let (call_write, res_write) = assistant_with_tool("w1", "write_file", "ok");
        let mut call_write = call_write;
        call_write.tool_calls[0].arguments =
            r#"{"path":"src/components/ContextRing.tsx","content":"..."}"#.to_string();
        call_write.content = "We decided to use monochrome tokens only.".to_string();

        let (call_denied, res_denied) = assistant_with_tool(
            "d1",
            "execute_command",
            "Error: tool execution was denied by the user",
        );

        let head = vec![
            system(),
            user("Implement M16 context accounting and ContextRing popover"),
            call_write,
            res_write,
            call_denied,
            res_denied,
            user("Wire /compact slash command too"),
        ];

        let structured = extract_structured_summary(
            &head,
            Some("Step 1: Add ContextCategoryBreakdown\nStep 2: Add /compact"),
            &["src-tauri/src/application/context_stats.rs".to_string()],
        );
        assert!(structured.goal.contains("Implement M16 context accounting"));
        assert!(structured
            .active_plan
            .contains("Add ContextCategoryBreakdown"));
        assert!(structured.key_decisions.contains("monochrome tokens"));
        assert!(structured.key_decisions.contains("denied"));
        assert!(structured
            .touched_files
            .contains(&"src/components/ContextRing.tsx".to_string()));
        assert!(structured
            .touched_files
            .contains(&"src-tauri/src/application/context_stats.rs".to_string()));
        assert!(structured.open_tasks.contains("/compact"));

        // Even if an LLM summary omits sections/files, merge preserves them.
        let merged = merge_summary_with_preserved_sections(Some("Short summary."), &structured);
        assert!(merged.contains("1. Goal"));
        assert!(merged.contains("2. Active Plan"));
        assert!(merged.contains("3. Key Decisions"));
        assert!(merged.contains("src/components/ContextRing.tsx"));
    }

    #[test]
    fn manual_compaction_reason_and_summary_template_sections() {
        assert_eq!(CompactionReason::Manual.as_str(), "manual");
        assert!(SUMMARY_TEMPLATE.contains("1. Goal"));
        assert!(SUMMARY_TEMPLATE.contains("2. Active Plan"));
        assert!(SUMMARY_TEMPLATE.contains("3. Key Decisions"));
        assert!(SUMMARY_TEMPLATE.contains("4. Touched Files / Open Tasks"));
    }
}
