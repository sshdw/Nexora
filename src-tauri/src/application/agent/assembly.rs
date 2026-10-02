//! Smart context assembly: stage-aware, priority-ordered construction of the
//! model's input window.
//!
//! The assembler budgets the opening window (and any carried tool
//! observations / history summary) against the canonical model window before
//! the first provider turn, so oversized assemblies shrink deterministically
//! instead of failing at the provider boundary:
//!
//! ```text
//! system identity -> task + role -> stage context -> tool outputs -> history
//!        (pinned)      (droppable)      (pinned)      (newest-N)     (lowest)
//! ```
//!
//! - [`SectionKind::ASSEMBLY_ORDER`] pins the section order: system identity
//!   first, the compacted history summary last.
//! - [`section_budgets`] derives per-section token budgets from
//!   [`context_limit_for`](crate::application::context_stats::context_limit_for),
//!   so lite-clamped models get proportionally smaller budgets automatically.
//! - Overflow trims lowest-priority first and oldest-first within a section:
//!   the history summary front-truncates, tool outputs drop oldest-first down
//!   to newest-N, then the task/role note front-truncates. System identity and
//!   the current stage section are never omitted; their content truncates only
//!   as the last resort before an explicit window, so the assembled context
//!   always fits the model limit.
//! - [`ProactiveHook`] fires the existing compaction path once when the
//!   requested size crosses the [`COMPACTION_THRESHOLD`](super::compaction::COMPACTION_THRESHOLD)
//!   fraction of the usable window; a still-overflowing second crossing
//!   surfaces the existing [`AgentError::ContextExhausted`] variant — no new
//!   error kinds.
//!
//! # Reuse (compose, never duplicate)
//!
//! | Concern | Reused source |
//! |---|---|
//! | Window resolution | `context_stats::context_limit_for` |
//! | Usable window, 0.8 threshold, 20k reserve | `compaction::{usable_context_tokens, should_compact, COMPACTION_THRESHOLD}` |
//! | Token estimates | `compaction::{estimate_tokens, message_tokens}` |
//! | Tool envelopes | `injection::envelope_tool_output` (unknown names collapse — hostile labels never echoed) |
//! | Lifecycle / stage / role vocabulary | `lifecycle::RunState`, `pipeline::PipelineStage` (+ its bound `AgentRole`), shared with `inspect::RunInspection` |
//! | Tool gating | `execution::model_info_for` (listing-side counterpart: `recommended_models`) |
//! | Terminal signal | `AgentError::ContextExhausted` |
//!
//! # Secret-freedom
//!
//! Sections carry caller text verbatim (prompts, observations, summaries), but
//! every derived string — stage text, budgets, hook outcomes, errors — uses
//! fixed vocabulary only (`as_str` names, counters, the
//! [`AgentError::ContextExhausted`] Display). No credentials, SQL, or key
//! material ever enter this module.
//!
//! # Staging note (wiring)
//!
//! The [`ToolOutput`] / [`AssemblyInput::tool_outputs`] and
//! [`AssemblyInput::history_summary`] sections currently have no production
//! caller: only the budgeted history loop in [`super::prompts`] feeds the
//! opening window today. The next wiring task extends that loop (or its
//! successor) to supply carried tool observations and the compacted history
//! summary through these sections rather than duplicating the share/trim
//! logic elsewhere.

use super::compaction;
use super::errors::AgentError;
use super::injection;
use super::lifecycle::RunState;
use super::pipeline::PipelineStage;
use crate::application::context_stats;
use crate::application::execution::{self, AiMessage, TokenUsage};

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// One priority-ordered context section.
///
/// Discriminant order is the assembly order (highest priority first); see
/// [`ASSEMBLY_ORDER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SectionKind {
    /// Fixed agent identity prompt. Never omitted.
    SystemIdentity,
    /// Task instruction plus role/memory notes (omission note, action trace).
    /// Front-truncates (oldest-first) under overflow; omittable when empty.
    TaskRole,
    /// Current pipeline stage framed in lifecycle vocabulary. Never omitted
    /// while a stage is attached; content truncates only as a last resort.
    StageContext,
    /// Enveloped tool observations, newest-N. Oldest-first drop under
    /// overflow; omitted entirely for non-tool models.
    ToolOutputs,
    /// Compacted history summary (lowest priority). Front-truncates
    /// (oldest-first) under overflow; omittable when empty.
    HistorySummary,
}

impl SectionKind {
    /// Canonical section name (fixed vocabulary for logs and tests).
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SystemIdentity => "system_identity",
            Self::TaskRole => "task_role",
            Self::StageContext => "stage_context",
            Self::ToolOutputs => "tool_outputs",
            Self::HistorySummary => "history_summary",
        }
    }
}

/// Pinned assembly order: identity first, compacted history last.
pub(crate) const ASSEMBLY_ORDER: [SectionKind; 5] = [
    SectionKind::SystemIdentity,
    SectionKind::TaskRole,
    SectionKind::StageContext,
    SectionKind::ToolOutputs,
    SectionKind::HistorySummary,
];

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

/// Share of the usable window reserved for the system-identity section.
pub(crate) const SYSTEM_IDENTITY_BUDGET_PERCENT: u64 = 10;
/// Share of the usable window reserved for the task + role section.
pub(crate) const TASK_ROLE_BUDGET_PERCENT: u64 = 15;
/// Share of the usable window reserved for the stage-context section.
pub(crate) const STAGE_CONTEXT_BUDGET_PERCENT: u64 = 15;
/// Share of the usable window reserved for enveloped tool outputs.
pub(crate) const TOOL_OUTPUTS_BUDGET_PERCENT: u64 = 35;
/// Share of the usable window reserved for the compacted history summary.
pub(crate) const HISTORY_SUMMARY_BUDGET_PERCENT: u64 = 25;

/// Per-section token budgets resolved for one provider/model pair.
///
/// Every field is a fixed share of [`usable`](Self::usable), so a
/// lite-clamped window yields proportionally smaller budgets through the same
/// [`context_limit_for`](crate::application::context_stats::context_limit_for)
/// function — no per-model tuning here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SectionBudgets {
    /// Usable prompt window after the output reserve (`0` = unbounded).
    pub usable: u64,
    /// Budget for [`SectionKind::SystemIdentity`].
    pub system_identity: u64,
    /// Budget for [`SectionKind::TaskRole`].
    pub task_role: u64,
    /// Budget for [`SectionKind::StageContext`].
    pub stage_context: u64,
    /// Budget for [`SectionKind::ToolOutputs`].
    pub tool_outputs: u64,
    /// Budget for [`SectionKind::HistorySummary`].
    pub history_summary: u64,
}

/// One share of `usable` in tokens (integer floor).
fn share(usable: u64, percent: u64) -> u64 {
    usable.saturating_mul(percent) / 100
}

fn section_budgets_for_usable(usable: u64) -> SectionBudgets {
    SectionBudgets {
        usable,
        system_identity: share(usable, SYSTEM_IDENTITY_BUDGET_PERCENT),
        task_role: share(usable, TASK_ROLE_BUDGET_PERCENT),
        stage_context: share(usable, STAGE_CONTEXT_BUDGET_PERCENT),
        tool_outputs: share(usable, TOOL_OUTPUTS_BUDGET_PERCENT),
        history_summary: share(usable, HISTORY_SUMMARY_BUDGET_PERCENT),
    }
}

/// Resolve per-section budgets for (`provider`, `model`) through the
/// canonical [`context_limit_for`](crate::application::context_stats::context_limit_for)
/// window (minus the compaction output reserve).
#[must_use]
pub(crate) fn section_budgets(provider: Option<&str>, model: Option<&str>) -> SectionBudgets {
    section_budgets_for_usable(compaction::usable_context_tokens(
        context_stats::context_limit_for(provider, model),
    ))
}

/// Whether (`provider`, `model`) may be offered tools.
///
/// Pure projection over [`execution::model_info_for`]: listed models are
/// tool-capable, unlisted IDs are conservatively not. The listing-side
/// counterpart is [`execution::recommended_models`] with `require_tools`.
#[must_use]
pub(crate) fn supports_tools_for(provider: &str, model: &str) -> bool {
    execution::model_info_for(provider, model).supports_tools
}

/// Resolve the effective model window: the explicit override when present
/// (`Some(0)` = unbounded — the legacy byte-identical path), otherwise the
/// canonical [`context_limit_for`](crate::application::context_stats::context_limit_for)
/// value for (`provider`, `model`).
#[must_use]
pub(crate) fn resolve_context_limit(provider: &str, model: &str, limit: Option<u64>) -> u64 {
    limit.unwrap_or_else(|| context_stats::context_limit_for(Some(provider), Some(model)))
}

// ---------------------------------------------------------------------------
// Stage context
// ---------------------------------------------------------------------------

/// Frame the current pipeline stage for the model input window.
///
/// Fixed vocabulary only: the stage name, its bound role, and the lifecycle
/// state — the same `as_str` values [`inspect`](super::inspect::RunInspection)
/// reports — inside one fixed sentence. Carries no content, credentials, or
/// SQL.
#[must_use]
pub(crate) fn stage_context_text(stage: PipelineStage, run_state: RunState) -> String {
    format!(
        "Current stage: {} (role: {}); run state: {}. Continue the task where the recent turns left off.",
        stage.as_str(),
        stage.role().as_str(),
        run_state.as_str()
    )
}

// ---------------------------------------------------------------------------
// Sizes
// ---------------------------------------------------------------------------

/// Estimated tokens over whole messages (visible text plus tool-call and
/// tool-result structure, via [`compaction::message_tokens`]).
#[must_use]
pub(crate) fn messages_size_tokens(messages: &[AiMessage]) -> u64 {
    messages
        .iter()
        .map(compaction::message_tokens)
        .fold(0_u64, u64::saturating_add)
}

/// Front-truncate `text` to `budget_tokens`, oldest-first (the newest tail
/// survives). A proportional first cut keeps the pass to at most a few
/// halvings; an empty result means even the newest sliver exceeds the budget.
fn truncate_front_to_budget(text: &str, budget_tokens: u64) -> String {
    if compaction::estimate_tokens(text) <= budget_tokens {
        return text.to_string();
    }
    let chars = text.chars().count();
    let chars_u64 = u64::try_from(chars).unwrap_or(u64::MAX);
    let estimate = compaction::estimate_tokens(text).max(1);
    let keep_u64 = chars_u64.saturating_mul(budget_tokens) / estimate;
    let keep = usize::try_from(keep_u64).unwrap_or(usize::MAX);
    let mut current: String = text.chars().skip(chars.saturating_sub(keep)).collect();
    while compaction::estimate_tokens(&current) > budget_tokens && !current.is_empty() {
        let total = current.chars().count();
        current = current
            .chars()
            .skip(total.saturating_sub(total / 2))
            .collect();
    }
    current
}

/// Tail-truncate `text` to `budget_tokens`, keeping the head (used for the
/// pinned sections' last-resort shrink, and for over-long current requests).
#[must_use]
pub(crate) fn truncate_tail_to_budget(text: &str, budget_tokens: u64) -> String {
    if compaction::estimate_tokens(text) <= budget_tokens {
        return text.to_string();
    }
    let chars = text.chars().count();
    let chars_u64 = u64::try_from(chars).unwrap_or(u64::MAX);
    let estimate = compaction::estimate_tokens(text).max(1);
    let keep_u64 = chars_u64.saturating_mul(budget_tokens) / estimate;
    let keep = usize::try_from(keep_u64).unwrap_or(usize::MAX);
    let mut current: String = text.chars().take(keep).collect();
    while compaction::estimate_tokens(&current) > budget_tokens && !current.is_empty() {
        let total = current.chars().count();
        current = current.chars().take(total / 2).collect();
    }
    current
}

/// Halve `text` keeping the head (one last-resort shrink step for pinned
/// content).
fn halve_tail(text: &str) -> String {
    text.chars().take(text.chars().count() / 2).collect()
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// One carried tool observation, oldest-first in the input slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolOutput {
    /// Tool that produced the observation (envelope label; unknown names
    /// collapse to the fixed label inside the envelope).
    pub tool_name: String,
    /// Raw observation text (framed, never rewritten, by the envelope).
    pub observation: String,
}

/// Inputs for one assembly pass.
#[derive(Debug)]
pub(crate) struct AssemblyInput<'a> {
    /// Internal provider name (window resolution).
    pub provider: &'a str,
    /// Model identifier (window resolution + tool gating).
    pub model: &'a str,
    /// Model window override (`None` = canonical resolution, `Some(0)` =
    /// unbounded passthrough).
    pub context_limit: Option<u64>,
    /// Lifecycle state framing the stage section.
    pub state: RunState,
    /// Current pipeline stage (`None` omits the stage section).
    pub stage: Option<PipelineStage>,
    /// Fixed agent identity prompt (never omitted).
    pub system_identity: &'a str,
    /// Task instruction plus role/memory notes (droppable).
    pub task_role: &'a str,
    /// Carried tool observations, oldest-first (newest-N under overflow;
    /// omitted for non-tool models).
    pub tool_outputs: &'a [ToolOutput],
    /// Compacted history summary, if any (lowest priority).
    pub history_summary: Option<&'a str>,
}

/// One assembled section: its kind plus the fitted content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssembledSection {
    /// Which section this is (assembly order = [`ASSEMBLY_ORDER`]).
    pub kind: SectionKind,
    /// Fitted section content.
    pub content: String,
}

/// The fitted input window plus its accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssembledContext {
    /// Fitted sections in [`ASSEMBLY_ORDER`]. The tools section is present
    /// only for tool-capable models with at least one retained observation;
    /// the history section only with a non-empty summary. Identity is always
    /// present, the stage section whenever a stage was attached.
    pub sections: Vec<AssembledSection>,
    /// Estimated tokens over the fitted sections.
    pub estimated_tokens: u64,
    /// Resolved model window (`0` = unbounded passthrough).
    pub context_limit: u64,
    /// Usable prompt window after the output reserve.
    pub usable_tokens: u64,
    /// True when the requested (pre-trim) size crossed the 0.8 proactive
    /// threshold: the caller should run the existing compaction path before
    /// sending.
    pub proactive_compaction_needed: bool,
    /// Tool observations dropped oldest-first by the budget (`0` when the
    /// section was omitted by model gating rather than by overflow).
    pub dropped_tool_outputs: usize,
}

impl AssembledContext {
    /// Borrow one fitted section's content, if present.
    #[must_use]
    pub(crate) fn section(&self, kind: SectionKind) -> Option<&str> {
        self.sections
            .iter()
            .find(|section| section.kind == kind)
            .map(|section| section.content.as_str())
    }
}

/// Estimated tokens over the five section texts.
fn sections_total(
    system_identity: &str,
    task_role: &str,
    stage: Option<&str>,
    tools: &[String],
    history: &str,
) -> u64 {
    let mut total = compaction::estimate_tokens(system_identity)
        .saturating_add(compaction::estimate_tokens(task_role));
    if let Some(stage_text) = stage {
        total = total.saturating_add(compaction::estimate_tokens(stage_text));
    }
    for tool in tools {
        total = total.saturating_add(compaction::estimate_tokens(tool));
    }
    total.saturating_add(compaction::estimate_tokens(history))
}

/// Keep the newest enveloped outputs fitting `budget_tokens` (oldest-first
/// drop). A lone over-budget newest output front-truncates to the share
/// instead of vanishing, so the newest observation is never silently lost.
fn newest_within_budget(
    items: Vec<String>,
    budget_tokens: u64,
    dropped: &mut usize,
) -> Vec<String> {
    if items.is_empty() {
        return items;
    }
    if budget_tokens == 0 {
        *dropped = dropped.saturating_add(items.len());
        return Vec::new();
    }
    let mut kept: Vec<String> = Vec::new();
    let mut kept_tokens = 0_u64;
    for item in items.iter().rev() {
        let estimate = compaction::estimate_tokens(item);
        if kept.is_empty() && estimate > budget_tokens {
            let truncated = truncate_front_to_budget(item, budget_tokens);
            if truncated.is_empty() {
                *dropped = dropped.saturating_add(items.len());
                return Vec::new();
            }
            kept.push(truncated);
            *dropped = dropped.saturating_add(items.len().saturating_sub(1));
            break;
        }
        if kept_tokens.saturating_add(estimate) > budget_tokens {
            *dropped = dropped.saturating_add(items.len().saturating_sub(kept.len()));
            break;
        }
        kept_tokens = kept_tokens.saturating_add(estimate);
        kept.push(item.clone());
    }
    kept.reverse();
    kept
}

/// Whether the requested size crosses the proactive threshold of `limit`.
///
/// Thin wrapper over [`compaction::should_compact`] treating the estimate as
/// the reported input tokens, so the 0.8 boundary, the strict-`>` trigger,
/// and the dormant `0` window all match the governor exactly.
fn crosses_proactive_threshold(estimated_tokens: u64, context_limit: u64) -> bool {
    compaction::should_compact(
        Some(TokenUsage {
            input_tokens: estimated_tokens,
            output_tokens: 0,
        }),
        context_limit,
        compaction::COMPACTION_THRESHOLD,
    )
}

/// Assemble the fitted input window for `input`.
///
/// Never fails and — for any non-degenerate window — the returned estimate
/// fits the usable tokens: overflow trims lowest-priority first and
/// oldest-first within a section (history front-truncate, then oldest tool
/// drops, then task/role front-truncate), leaving system identity and the
/// current stage for last-resort content truncation only. A `Some(0)` window
/// (and a window at/below the output reserve) passes everything through
/// untouched with the hook dormant, mirroring the governor.
#[must_use]
pub(crate) fn assemble(input: &AssemblyInput<'_>) -> AssembledContext {
    let context_limit = resolve_context_limit(input.provider, input.model, input.context_limit);
    let usable = compaction::usable_context_tokens(context_limit);
    let supports_tools = supports_tools_for(input.provider, input.model);

    let enveloped: Vec<String> = if supports_tools {
        input
            .tool_outputs
            .iter()
            .map(|output| injection::envelope_tool_output(&output.tool_name, &output.observation))
            .collect()
    } else {
        Vec::new()
    };
    let history_full = input.history_summary.unwrap_or("");

    // Unbounded or degenerate windows pass through with the hook dormant.
    if context_limit == 0 || usable == 0 {
        let estimated = sections_total(
            input.system_identity,
            input.task_role,
            None,
            &enveloped,
            history_full,
        );
        let stage_text = input
            .stage
            .map(|stage| stage_context_text(stage, input.state));
        let estimated =
            estimated.saturating_add(stage_text.as_deref().map_or(0, compaction::estimate_tokens));
        return render(
            input,
            input.system_identity.to_string(),
            input.task_role.to_string(),
            stage_text,
            &enveloped,
            history_full.to_string(),
            estimated,
            context_limit,
            usable,
            false,
            0,
        );
    }

    let requested = sections_total(
        input.system_identity,
        input.task_role,
        input
            .stage
            .map(|stage| stage_context_text(stage, input.state))
            .as_deref(),
        &enveloped,
        history_full,
    );
    let proactive_compaction_needed = crosses_proactive_threshold(requested, context_limit);

    let budgets = section_budgets_for_usable(usable);
    let fitted = fit_sections(
        input.system_identity.to_string(),
        input.task_role,
        input
            .stage
            .map(|stage| stage_context_text(stage, input.state)),
        enveloped,
        history_full,
        &budgets,
        usable,
    );

    render(
        input,
        fitted.system_identity,
        fitted.task_role,
        fitted.stage_text,
        &fitted.tools,
        fitted.history,
        fitted.estimated,
        context_limit,
        usable,
        proactive_compaction_needed,
        fitted.dropped_tools,
    )
}

/// Fitted section texts after the share and global passes.
struct FittedSections {
    system_identity: String,
    task_role: String,
    stage_text: Option<String>,
    tools: Vec<String>,
    history: String,
    estimated: u64,
    dropped_tools: usize,
}

/// Trim sections to the window: first each droppable section toward its
/// share (history front-truncate, tool oldest-drops to newest-N, task/role
/// front-truncate), then — while still overflowing — lowest priority first
/// down to empty, leaving system identity and the stage for last-resort
/// content truncation only (never omitted).
fn fit_sections(
    system_identity: String,
    task_role: &str,
    stage_text: Option<String>,
    tools: Vec<String>,
    history: &str,
    budgets: &SectionBudgets,
    usable: u64,
) -> FittedSections {
    // Phase A: per-section shares (newest-N tools, oldest-first truncations).
    let mut history = truncate_front_to_budget(history, budgets.history_summary);
    let mut dropped_tools = 0_usize;
    let mut tools = newest_within_budget(tools, budgets.tool_outputs, &mut dropped_tools);
    let mut task_role = truncate_front_to_budget(task_role, budgets.task_role);
    let mut stage_text = stage_text;
    let mut system_identity = system_identity;

    // Phase B: global fit, lowest priority first; pinned sections shrink only
    // as the last resort and are never omitted.
    let mut estimated = sections_total(
        &system_identity,
        &task_role,
        stage_text.as_deref(),
        &tools,
        &history,
    );
    if estimated > usable && !history.is_empty() {
        history.clear();
        estimated = sections_total(
            &system_identity,
            &task_role,
            stage_text.as_deref(),
            &tools,
            &history,
        );
    }
    while estimated > usable && !tools.is_empty() {
        tools.remove(0);
        dropped_tools = dropped_tools.saturating_add(1);
        estimated = sections_total(
            &system_identity,
            &task_role,
            stage_text.as_deref(),
            &tools,
            &history,
        );
    }
    if estimated > usable && !task_role.is_empty() {
        task_role.clear();
        estimated = sections_total(
            &system_identity,
            &task_role,
            stage_text.as_deref(),
            &tools,
            &history,
        );
    }
    while estimated > usable && stage_text.as_deref().is_some_and(|text| !text.is_empty()) {
        stage_text = stage_text.map(|text| halve_tail(&text));
        estimated = sections_total(
            &system_identity,
            &task_role,
            stage_text.as_deref(),
            &tools,
            &history,
        );
    }
    while estimated > usable && !system_identity.is_empty() {
        system_identity = halve_tail(&system_identity);
        estimated = sections_total(
            &system_identity,
            &task_role,
            stage_text.as_deref(),
            &tools,
            &history,
        );
    }
    FittedSections {
        system_identity,
        task_role,
        stage_text,
        tools,
        history,
        estimated,
        dropped_tools,
    }
}

/// Render the fitted texts as ordered sections, omitting empties except the
/// pinned identity (and an attached stage, which is never dropped).
#[allow(clippy::too_many_arguments)]
fn render(
    input: &AssemblyInput<'_>,
    system_identity: String,
    task_role: String,
    stage_text: Option<String>,
    tools: &[String],
    history: String,
    estimated: u64,
    context_limit: u64,
    usable: u64,
    proactive: bool,
    dropped_tool_outputs: usize,
) -> AssembledContext {
    let supports_tools = supports_tools_for(input.provider, input.model);
    let mut sections = Vec::with_capacity(ASSEMBLY_ORDER.len());
    sections.push(AssembledSection {
        kind: SectionKind::SystemIdentity,
        content: system_identity,
    });
    if !task_role.is_empty() {
        sections.push(AssembledSection {
            kind: SectionKind::TaskRole,
            content: task_role,
        });
    }
    if input.stage.is_some() {
        sections.push(AssembledSection {
            kind: SectionKind::StageContext,
            content: stage_text.unwrap_or_default(),
        });
    }
    if supports_tools && !tools.is_empty() {
        sections.push(AssembledSection {
            kind: SectionKind::ToolOutputs,
            content: tools.join("\n\n"),
        });
    }
    if !history.is_empty() {
        sections.push(AssembledSection {
            kind: SectionKind::HistorySummary,
            content: history,
        });
    }
    debug_assert!(
        sections
            .iter()
            .map(|section| section.kind)
            .eq(ASSEMBLY_ORDER
                .into_iter()
                .filter(|kind| sections.iter().any(|section| section.kind == *kind))),
        "rendered sections keep assembly order"
    );
    AssembledContext {
        sections,
        estimated_tokens: estimated,
        context_limit,
        usable_tokens: usable,
        proactive_compaction_needed: proactive,
        dropped_tool_outputs,
    }
}

// ---------------------------------------------------------------------------
// Proactive hook
// ---------------------------------------------------------------------------

/// Once-only proactive-compaction hook over the assembled size.
///
/// The first [`poll`](Self::poll) crossing the 0.8 threshold returns `Ok(true)`
/// — run the existing compaction path. Later polls stay quiet (`Ok(false)`
/// below the threshold); a *persisted* crossing after the hook fired returns
/// the existing terminal [`AgentError::ContextExhausted`] instead of firing
/// again, so recovery exhaustion keeps its distinct signal and no new error
/// kind is introduced.
#[derive(Debug, Default)]
pub(crate) struct ProactiveHook {
    fired: bool,
}

impl ProactiveHook {
    /// A hook that has never fired.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Poll the hook against `estimated_tokens` under `context_limit`.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::ContextExhausted`] when the size still crosses
    /// the threshold after the hook already fired (recovery exhausted —
    /// terminal, distinct from the retryable provider overflow).
    pub(crate) fn poll(
        &mut self,
        estimated_tokens: u64,
        context_limit: u64,
    ) -> Result<bool, AgentError> {
        if !crosses_proactive_threshold(estimated_tokens, context_limit) {
            return Ok(false);
        }
        if self.fired {
            return Err(AgentError::ContextExhausted);
        }
        self.fired = true;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::compaction;
    use crate::application::agent::prompts::AGENT_SYSTEM_PROMPT;
    use crate::application::context_stats;

    fn tool_output(name: &str, observation: &str) -> ToolOutput {
        ToolOutput {
            tool_name: name.to_string(),
            observation: observation.to_string(),
        }
    }

    fn full_input<'a>(tools: &'a [ToolOutput], history: Option<&'a str>) -> AssemblyInput<'a> {
        AssemblyInput {
            provider: "openai",
            model: "gpt-5.6-terra",
            context_limit: None,
            state: RunState::Running,
            stage: Some(PipelineStage::Act),
            system_identity: AGENT_SYSTEM_PROMPT,
            task_role: "do the thing",
            tool_outputs: tools,
            history_summary: history,
        }
    }

    #[test]
    fn section_order_is_pinned_identity_first_history_last() {
        let tools = [
            tool_output("read_file", "content-a"),
            tool_output("search_files", "content-b"),
        ];
        let assembled = assemble(&full_input(&tools, Some("earlier work summary")));

        let kinds: Vec<SectionKind> = assembled
            .sections
            .iter()
            .map(|section| section.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                SectionKind::SystemIdentity,
                SectionKind::TaskRole,
                SectionKind::StageContext,
                SectionKind::ToolOutputs,
                SectionKind::HistorySummary,
            ],
            "assembly order is the pinned contract"
        );
        for (index, kind) in kinds.iter().enumerate() {
            assert_eq!(
                ASSEMBLY_ORDER[index], *kind,
                "position {index} matches ASSEMBLY_ORDER"
            );
        }
        assert!(
            assembled
                .section(SectionKind::SystemIdentity)
                .expect("identity present")
                .starts_with(AGENT_SYSTEM_PROMPT),
            "identity opens the window verbatim"
        );
        let stage = assembled
            .section(SectionKind::StageContext)
            .expect("stage present");
        assert!(
            stage.contains(PipelineStage::Act.as_str()),
            "stage names itself: {stage}"
        );
        assert!(
            stage.contains(PipelineStage::Act.role().as_str()),
            "stage names its bound role: {stage}"
        );
        let tools_text = assembled
            .section(SectionKind::ToolOutputs)
            .expect("tools present");
        assert!(tools_text.contains("content-a") && tools_text.contains("content-b"));
        assert!(
            tools_text.contains(injection::ENVELOPE_HEADER_PREFIX),
            "tool observations reuse the injection envelopes"
        );
        assert_eq!(assembled.dropped_tool_outputs, 0);
        assert!(assembled.estimated_tokens <= assembled.usable_tokens);
        assert!(
            !assembled.proactive_compaction_needed,
            "a small fitting assembly asks for no compaction"
        );
    }

    #[test]
    fn overflow_drops_oldest_low_priority_first_and_pins_identity_and_stage() {
        // ~480k chars of tool output plus a 100k history against the 108k
        // usable window: genuine overflow that must trim oldest-first.
        let big = "x".repeat(80_000);
        let tools: Vec<ToolOutput> = (0..6)
            .map(|index| tool_output("read_file", &format!("MARKER-{index}-{big}")))
            .collect();
        let history = format!("HISTORY-OLD-START-{}", "y".repeat(100_000));
        let assembled = assemble(&full_input(&tools, Some(&history)));

        assert!(
            assembled.estimated_tokens <= assembled.usable_tokens,
            "fitted {} into {}",
            assembled.estimated_tokens,
            assembled.usable_tokens
        );
        assert!(
            assembled.proactive_compaction_needed,
            "an overflowing request still asks for the compaction path"
        );
        assert!(
            assembled.dropped_tool_outputs > 0,
            "oldest tools must drop, dropped={}",
            assembled.dropped_tool_outputs
        );
        let tools_text = assembled
            .section(SectionKind::ToolOutputs)
            .expect("newest tools retained");
        assert!(
            tools_text.contains("MARKER-5"),
            "the newest tool output survives"
        );
        assert!(
            !tools_text.contains("MARKER-0"),
            "the oldest tool output drops first"
        );
        assert!(
            assembled
                .section(SectionKind::SystemIdentity)
                .expect("identity never dropped")
                .starts_with(AGENT_SYSTEM_PROMPT),
            "system identity is never dropped"
        );
        let stage = assembled
            .section(SectionKind::StageContext)
            .expect("current stage never dropped");
        assert!(stage.contains(PipelineStage::Act.as_str()));
    }

    #[test]
    fn proactive_hook_fires_exactly_once_then_signals_exhaustion() {
        let limit = context_stats::context_limit_for(Some("openai"), Some("gpt-5.6-terra"));
        assert_eq!(limit, context_stats::OPENAI_CONTEXT_LIMIT);
        let usable = compaction::usable_context_tokens(limit);
        let at_threshold = usable * 8 / 10;
        let over_threshold = at_threshold + 1;

        let mut hook = ProactiveHook::new();
        assert!(
            !hook
                .poll(at_threshold, limit)
                .expect("exactly 0.8 proceeds"),
            "the strict-> trigger stays quiet at exactly 0.8"
        );
        assert!(
            hook.poll(over_threshold, limit)
                .expect("first crossing compacts"),
            "the first crossing fires the compaction path"
        );
        let err = hook
            .poll(over_threshold, limit)
            .expect_err("a persisted crossing must not fire again");
        assert!(
            matches!(err, AgentError::ContextExhausted),
            "recovery exhaustion keeps the distinct terminal signal, got {err:?}"
        );

        let mut fresh = ProactiveHook::new();
        assert!(!fresh.poll(0, limit).expect("zero proceeds"));
        assert!(
            !fresh.poll(u64::MAX, 0).expect("dormant window proceeds"),
            "unknown model windows keep the hook dormant"
        );
    }

    #[test]
    fn lite_budgets_scale_proportionally_with_the_same_limit_function() {
        let full = section_budgets(Some("gemini"), Some("gemini-3.6-flash"));
        let lite = section_budgets(Some("gemini"), Some("gemini-3.1-flash-lite"));
        assert_eq!(
            full.usable,
            context_stats::GEMINI_CONTEXT_LIMIT - compaction::COMPACTION_RESERVED_TOKENS
        );
        assert_eq!(
            lite.usable,
            context_stats::GEMINI_LITE_CONTEXT_LIMIT - compaction::COMPACTION_RESERVED_TOKENS
        );
        assert!(lite.usable < full.usable, "lite clamps the window");

        for (name, lite_budget, full_budget) in [
            (
                "system_identity",
                lite.system_identity,
                full.system_identity,
            ),
            ("task_role", lite.task_role, full.task_role),
            ("stage_context", lite.stage_context, full.stage_context),
            ("tool_outputs", lite.tool_outputs, full.tool_outputs),
            (
                "history_summary",
                lite.history_summary,
                full.history_summary,
            ),
        ] {
            assert!(
                lite_budget < full_budget,
                "{name}: lite {lite_budget} must sit below full {full_budget}"
            );
        }
        // Shares are pinned fractions of the usable window: cross-multiplied
        // budgets agree up to integer flooring (at most one usable-window
        // token of slack on either side).
        for (name, lite_budget, full_budget) in [
            (
                "system_identity",
                lite.system_identity,
                full.system_identity,
            ),
            ("task_role", lite.task_role, full.task_role),
            ("stage_context", lite.stage_context, full.stage_context),
            ("tool_outputs", lite.tool_outputs, full.tool_outputs),
            (
                "history_summary",
                lite.history_summary,
                full.history_summary,
            ),
        ] {
            let scaled_lite = lite_budget.saturating_mul(full.usable);
            let scaled_full = full_budget.saturating_mul(lite.usable);
            assert!(
                scaled_lite.abs_diff(scaled_full) <= full.usable,
                "{name}: lite {lite_budget}/{usable_lite} must scale with full {full_budget}/{usable_full}",
                usable_lite = lite.usable,
                usable_full = full.usable,
            );
        }
        let total = full.system_identity
            + full.task_role
            + full.stage_context
            + full.tool_outputs
            + full.history_summary;
        assert!(total <= full.usable, "budgets never exceed the window");
        assert!(
            full.usable - total < 5,
            "shares partition the window up to integer flooring"
        );
    }

    #[test]
    fn non_tool_models_omit_the_tools_section() {
        let tools = [tool_output("read_file", "abc")];
        let listed = AssemblyInput {
            provider: "openai",
            model: "gpt-5.6-terra",
            context_limit: None,
            state: RunState::Running,
            stage: None,
            system_identity: AGENT_SYSTEM_PROMPT,
            task_role: "task",
            tool_outputs: &tools,
            history_summary: None,
        };
        let assembled = assemble(&listed);
        assert!(
            assembled.section(SectionKind::ToolOutputs).is_some(),
            "listed models keep the tools section"
        );

        let unlisted = AssemblyInput {
            model: "mystery-model-9",
            ..listed
        };
        let gated = assemble(&unlisted);
        assert_eq!(
            gated.section(SectionKind::ToolOutputs),
            None,
            "non-tool models carry no tools section"
        );
        assert_eq!(
            gated.dropped_tool_outputs, 0,
            "gating omits, it never drops"
        );

        // The listing-side counterpart agrees: tool-gated recommendations
        // keep the listed model and exclude the unknown ID.
        let recommended = execution::recommended_models("openai", true);
        assert!(recommended.contains(&"gpt-5.6-terra".to_string()));
        assert!(!recommended.contains(&"mystery-model-9".to_string()));
    }

    #[test]
    fn adversarial_sizes_always_fit_with_identity_and_stage_pinned() {
        // ~2.5MB of carried context against the lite-clamped window.
        let huge_task = "t".repeat(500_000);
        let tools: Vec<ToolOutput> = (0..20)
            .map(|index| {
                tool_output(
                    "read_file",
                    &format!("BLOB-{index}-{}", "z".repeat(100_000)),
                )
            })
            .collect();
        let history = "h".repeat(500_000);
        let input = AssemblyInput {
            provider: "gemini",
            model: "gemini-3.1-flash-lite",
            context_limit: None,
            state: RunState::Running,
            stage: Some(PipelineStage::Review),
            system_identity: AGENT_SYSTEM_PROMPT,
            task_role: &huge_task,
            tool_outputs: &tools,
            history_summary: Some(&history),
        };
        let assembled = assemble(&input);

        assert!(
            assembled.estimated_tokens <= assembled.usable_tokens,
            "adversarial input fits: {} into {}",
            assembled.estimated_tokens,
            assembled.usable_tokens
        );
        assert!(
            assembled
                .section(SectionKind::SystemIdentity)
                .expect("identity present")
                .starts_with(AGENT_SYSTEM_PROMPT),
            "system identity survives adversarial overflow"
        );
        assert!(
            assembled
                .section(SectionKind::StageContext)
                .expect("stage present")
                .contains(PipelineStage::Review.as_str()),
            "the current stage survives adversarial overflow"
        );
        assert!(assembled.proactive_compaction_needed);
    }

    #[test]
    fn unbounded_window_passes_through_with_dormant_hook() {
        let tools = [tool_output("unknown_tool_xyz", "kept")];
        let input = AssemblyInput {
            context_limit: Some(0),
            tool_outputs: &tools,
            history_summary: Some("summary"),
            ..full_input(&tools, Some("summary"))
        };
        let assembled = assemble(&input);

        assert_eq!(assembled.context_limit, 0);
        assert_eq!(assembled.dropped_tool_outputs, 0);
        assert!(!assembled.proactive_compaction_needed);
        assert_eq!(
            assembled.section(SectionKind::TaskRole),
            Some("do the thing")
        );
        assert_eq!(
            assembled.section(SectionKind::HistorySummary),
            Some("summary")
        );
        let tools_text = assembled
            .section(SectionKind::ToolOutputs)
            .expect("tools pass through unbounded");
        assert!(tools_text.contains("kept"));
        assert!(
            !tools_text.contains("unknown_tool_xyz"),
            "even unbounded, hostile tool names never enter the envelope"
        );
    }
}
