//! Conversation context stats: read-only per-conversation token/cost
//! breakdown (stats + diff render only; no agent behavior change).
//!
//! Per-turn token usage is persisted on `agent_steps` (`input_tokens` /
//! `output_tokens`, v14, NEX-AGENT-005) from the provider-reported
//! `response.usage`; turns whose provider omitted usage read back NULL and
//! are excluded from the sums with `has_token_data` set only when at least
//! one turn reported. Cost is computed through the existing
//! `agent::pricing` policy so the math stays in one place.
//!
//! Context windows resolve through [`context_limit_for`], whose per-model
//! arm is derived from the provider `SUPPORTED_MODELS` lists (NEX-AGENT-010)
//! rather than a restated ID table, so the two cannot drift.

use serde::Serialize;

use crate::application::agent::pricing;
use crate::application::conversations::{ConversationError, Result};
use crate::infrastructure::database::Database;
use crate::infrastructure::providers::{anthropic, gemini, openai};
use crate::infrastructure::repository::agent_runs::AgentRunRepository;
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::messages::MessageRepository;
use crate::infrastructure::repository::providers::ProviderRepository;

/// Context window for OpenAI-hosted models (conservative documented default).
pub(crate) const OPENAI_CONTEXT_LIMIT: u64 = 128_000;
/// Context window for Anthropic-hosted models (conservative documented default).
pub(crate) const ANTHROPIC_CONTEXT_LIMIT: u64 = 200_000;
/// Context window for Gemini-hosted models (conservative documented default).
pub(crate) const GEMINI_CONTEXT_LIMIT: u64 = 1_048_576;
/// Conservative baseline for unverified gemini-lite windows until
/// live-verified; architecture effective = min(requested, capability).
///
/// Live-verified 2026-10-03 via the zero-token `GET
/// /v1beta/models/{modelId}` metadata endpoint (header auth, key from the
/// OS keyring through `CredentialStore`, never logged): all four lite IDs
/// below report `inputTokenLimit = 1048576`, so the clamp is raised to the
/// verified 1M window. Kept as a separate arm (rather than folding into
/// [`GEMINI_CONTEXT_LIMIT`]) so a future re-verification that splits the
/// data touches one place.
pub(crate) const GEMINI_LITE_CONTEXT_LIMIT: u64 = 1_048_576;
/// Fallback when the provider/model is unknown or has no committed window.
pub(crate) const DEFAULT_CONTEXT_LIMIT: u64 = 128_000;

/// Resolve the documented context limit for `provider`/`model`.
///
/// Provider names are the internal `providers.name` values (`openai`,
/// `anthropic`, `gemini`, ...); compat providers ride the OpenAI-compatible
/// path and fall back to the default. Known model IDs resolve first via
/// [`model_context_limit`] (currently pinned to their provider's
/// conservative window — see below); anything unlisted falls through to the
/// provider fallback, then the `gemini`-substring rule, then the default.
///
/// Code is the source of truth for windows: `docs/Nexora-AI-Model-Catalog-*`
/// is research-only and claims larger per-model windows (e.g. ~1.05M) that
/// diverge from these conservative defaults, so no per-model bump is applied
/// without live verification. A smaller (earlier-compacting) window is the
/// safe direction for the proactive trigger.
#[must_use]
pub(crate) fn context_limit_for(provider: Option<&str>, model: Option<&str>) -> u64 {
    if let Some(limit) = model.and_then(model_context_limit) {
        return limit;
    }
    match provider {
        Some("anthropic") => ANTHROPIC_CONTEXT_LIMIT,
        Some("gemini") => GEMINI_CONTEXT_LIMIT,
        Some("openai") => OPENAI_CONTEXT_LIMIT,
        _ => {
            if model.is_some_and(|m| m.contains("gemini")) {
                GEMINI_CONTEXT_LIMIT
            } else {
                DEFAULT_CONTEXT_LIMIT
            }
        }
    }
}

/// Per-model window derived from the provider `SUPPORTED_MODELS` lists
/// (`infrastructure::providers::{openai,anthropic,gemini}`), so the window
/// table cannot drift from the supported-model surface (NEX-AGENT-010).
///
/// Each listed ID maps to its provider's conservative window constant above —
/// pinning the value per provider (so a future verified bump touches one
/// arm) without inventing numbers from the research-only catalog. The
/// OpenAI-compatible shortlists (`xkiro`, `openrouter`, `nvidia`,
/// `opencode_zen`) ride the shared OpenAI-compatible executor, so they
/// resolve to the conservative `OpenAI` window; the user-configured
/// `openai_compat` endpoint carries no shortlist and falls through to the
/// provider fallback below. Returns `None` for unlisted IDs (honest
/// unknown — never a guessed window) so [`context_limit_for`] falls through
/// to the provider fallback.
#[must_use]
fn model_context_limit(model: &str) -> Option<u64> {
    if openai::SUPPORTED_MODELS.contains(&model)
        || openai::XKIRO_MODELS.contains(&model)
        || openai::OPENROUTER_MODELS.contains(&model)
        || openai::NVIDIA_MODELS.contains(&model)
        || openai::OPENCODE_ZEN_MODELS.contains(&model)
    {
        Some(OPENAI_CONTEXT_LIMIT)
    } else if anthropic::SUPPORTED_MODELS.contains(&model) {
        Some(ANTHROPIC_CONTEXT_LIMIT)
    } else if gemini::SUPPORTED_MODELS.contains(&model) {
        // Live-verified 2026-10-03 (metadata endpoint, zero-token):
        // every lite ID reports `inputTokenLimit = 1048576`, so the lite
        // subset resolves to the verified [`GEMINI_LITE_CONTEXT_LIMIT`].
        if gemini::GEMINI_LITE_MODELS.contains(&model) {
            Some(GEMINI_LITE_CONTEXT_LIMIT)
        } else {
            Some(GEMINI_CONTEXT_LIMIT)
        }
    } else {
        None
    }
}

/// Read-only per-conversation context stats (`snake_case` over IPC).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ConversationContextStats {
    /// Owning conversation id.
    pub conversation_id: i64,
    /// Conversation title (`sessions` label in the UI).
    pub title: String,
    /// Internal provider name of the newest attributed assistant message.
    pub provider: Option<String>,
    /// Display label of that provider row.
    pub provider_display: Option<String>,
    /// Model of the newest attributed assistant message.
    pub model: Option<String>,
    /// Documented context window for the resolved provider/model.
    pub context_limit: u64,
    /// Total persisted usage tokens (`input + output`) across the
    /// conversation's runs (`0` when no turn reported usage).
    pub total_tokens: u64,
    /// Persisted input tokens across the conversation's runs.
    pub input_tokens: u64,
    /// Persisted output tokens across the conversation's runs.
    pub output_tokens: u64,
    /// Reasoning tokens (always `0`; no persisted source).
    pub reasoning_tokens: u64,
    /// Cache-read tokens (always `0`; no persisted source).
    pub cache_read_tokens: u64,
    /// Cache-write tokens (always `0`; no persisted source).
    pub cache_write_tokens: u64,
    /// Whether any persisted step carries provider-reported usage
    /// (`false` — "n/a" in the UI — when no turn reported any).
    pub has_token_data: bool,
    /// Billed cost in micro-USD via `pricing` for the summed usage.
    pub total_cost_micro_usd: u64,
    /// `total_tokens / context_limit * 100.0` (`0.0` while usage is absent).
    pub usage_percent: f64,
    /// Total persisted messages.
    pub message_count: u64,
    /// Persisted `role = 'user'` messages.
    pub user_message_count: u64,
    /// Persisted `role = 'assistant'` messages.
    pub assistant_message_count: u64,
    /// Persisted `agent_steps` with `kind = 'tool_call'` across the
    /// conversation's runs (drives the "tool calls" breakdown slice).
    pub tool_call_count: u64,
    /// Persisted non-tool agent steps across the conversation's runs
    /// (drives the "other" breakdown slice).
    pub other_step_count: u64,
    /// Conversation creation timestamp (Unix seconds).
    pub created_at: i64,
    /// Conversation last-activity timestamp (Unix seconds).
    pub updated_at: i64,
}

/// Compute read-only context stats for `conversation_id`.
///
/// Counts come from persisted `messages`; provider/model from the newest
/// assistant message carrying attribution; tool/other step counts and token
/// sums from the conversation's persisted agent runs. Token sums cover only
/// turns whose provider reported usage (`agent_steps.input_tokens` /
/// `output_tokens` non-NULL); usage-less turns contribute nothing and
/// `has_token_data` is `false` when no turn reported any (never estimated
/// silently). Cost goes through `pricing::cost_micro_for_model` for the
/// resolved model, so per-conversation cost falls out of the persisted
/// usage through the existing policy table.
///
/// # Errors
///
/// Returns [`ConversationError::NotFound`] when the conversation does not
/// exist, or [`ConversationError::Database`] when any query fails.
pub(crate) fn conversation_context_stats(
    db: &Database,
    conversation_id: i64,
) -> Result<ConversationContextStats> {
    let conversations = ConversationRepository::new(db);
    let conversation = conversations
        .read(conversation_id)?
        .ok_or(ConversationError::NotFound {
            id: conversation_id,
        })?;

    let messages = MessageRepository::new(db).list_by_conversation(conversation_id)?;
    let mut user_count: u64 = 0;
    let mut assistant_count: u64 = 0;
    for message in &messages {
        if message.role == "user" {
            user_count = user_count.saturating_add(1);
        } else if message.role == "assistant" {
            assistant_count = assistant_count.saturating_add(1);
        }
    }

    // Newest attributed assistant message decides provider/model display.
    let mut provider: Option<String> = None;
    let mut provider_display: Option<String> = None;
    let mut model: Option<String> = None;
    let providers = ProviderRepository::new(db);
    for message in messages.iter().rev() {
        if message.role != "assistant" {
            continue;
        }
        let candidate_model = message.model_name.clone();
        let candidate_provider = match message.provider_id {
            Some(id) => providers.read(id)?.map(|p| (p.name, p.display_name)),
            None => None,
        };
        if candidate_model.is_some() || candidate_provider.is_some() {
            model = candidate_model;
            if let Some((name, display)) = candidate_provider {
                provider = Some(name);
                provider_display = Some(display);
            }
            break;
        }
    }

    let context_limit = context_limit_for(provider.as_deref(), model.as_deref());

    let runs = AgentRunRepository::new(db).list_runs_by_conversation(conversation_id)?;
    let steps_repo = AgentRunRepository::new(db);
    let mut tool_call_count: u64 = 0;
    let mut other_step_count: u64 = 0;
    // Per-turn usage summed across the conversation's runs. A step counts
    // only when the provider reported usage for that turn (both columns
    // non-NULL — they are always written together); usage-less turns are
    // skipped honestly rather than counted as zero-usage evidence.
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut has_token_data = false;
    for run in &runs {
        for step in steps_repo.list_steps(run.id)? {
            if step.kind == "tool_call" {
                tool_call_count = tool_call_count.saturating_add(1);
            } else {
                other_step_count = other_step_count.saturating_add(1);
            }
            if let (Some(input), Some(output)) = (step.input_tokens, step.output_tokens) {
                has_token_data = true;
                input_tokens = input_tokens.saturating_add(input);
                output_tokens = output_tokens.saturating_add(output);
            }
        }
    }
    let total_tokens = input_tokens.saturating_add(output_tokens);
    let total_cost_micro_usd =
        pricing::cost_micro_for_model(model.as_deref().unwrap_or(""), input_tokens, output_tokens);
    let usage_percent = if context_limit == 0 {
        0.0
    } else {
        // `u32 -> f64` is exact, so this avoids any precision-lossy cast;
        // limits and totals saturate at `u32::MAX` far above real windows.
        let total_f = f64::from(u32::try_from(total_tokens).unwrap_or(u32::MAX));
        let limit_f = f64::from(u32::try_from(context_limit).unwrap_or(u32::MAX));
        (total_f / limit_f) * 100.0
    };

    Ok(ConversationContextStats {
        conversation_id,
        title: conversation.title,
        provider,
        provider_display,
        model,
        context_limit,
        total_tokens,
        input_tokens,
        output_tokens,
        reasoning_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        has_token_data,
        total_cost_micro_usd,
        usage_percent,
        message_count: user_count.saturating_add(assistant_count),
        user_message_count: user_count,
        assistant_message_count: assistant_count,
        tool_call_count,
        other_step_count,
        created_at: conversation.created_at,
        updated_at: conversation.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::pricing::{
        cost_micro_for_model, POLICY_DEFAULT_INPUT_MICRO_PER_1M,
    };
    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;
    use crate::infrastructure::repository::conversations::ConversationRepository;
    use crate::infrastructure::repository::messages::MessageRepository;
    use crate::infrastructure::repository::providers::ProviderRepository;

    fn seed_conversation(db: &Database, title: &str) -> i64 {
        ConversationRepository::new(db)
            .create(title, "active")
            .expect("create conversation")
    }

    #[test]
    fn empty_conversation_reports_zero_counts_and_na_tokens() {
        let db = in_memory_database();
        let id = seed_conversation(&db, "Empty");

        let stats = conversation_context_stats(&db, id).expect("stats");

        assert_eq!(stats.conversation_id, id);
        assert_eq!(stats.title, "Empty");
        assert_eq!(stats.message_count, 0);
        assert_eq!(stats.user_message_count, 0);
        assert_eq!(stats.assistant_message_count, 0);
        assert_eq!(stats.total_tokens, 0);
        assert_eq!(stats.input_tokens, 0);
        assert_eq!(stats.output_tokens, 0);
        assert_eq!(stats.reasoning_tokens, 0);
        assert_eq!(stats.cache_read_tokens, 0);
        assert_eq!(stats.cache_write_tokens, 0);
        assert!(!stats.has_token_data, "absent usage must be flagged n/a");
        assert_eq!(stats.total_cost_micro_usd, 0);
        assert!(
            stats.usage_percent.abs() < f64::EPSILON,
            "usage must be ~0 with no persisted tokens, got {}",
            stats.usage_percent
        );
        assert_eq!(stats.provider, None);
        assert_eq!(stats.model, None);
        assert_eq!(stats.context_limit, DEFAULT_CONTEXT_LIMIT);
        assert_eq!(stats.tool_call_count, 0);
        assert_eq!(stats.other_step_count, 0);
        assert!(stats.created_at > 0);
        assert!(stats.updated_at >= stats.created_at);
    }

    #[test]
    fn counts_provider_model_limit_and_cost_are_exact() {
        let db = in_memory_database();
        let provider_id = ProviderRepository::new(&db)
            .create("openai", "OpenAI")
            .expect("create provider");
        let id = seed_conversation(&db, "Review session");
        let messages = MessageRepository::new(&db);
        messages
            .create(id, "user", "hello", None, None)
            .expect("user message");
        messages
            .create(
                id,
                "assistant",
                "hi there",
                Some(provider_id),
                Some("gpt-5.6-terra"),
            )
            .expect("assistant message");
        messages
            .create(id, "user", "again", None, None)
            .expect("second user message");

        let stats = conversation_context_stats(&db, id).expect("stats");

        assert_eq!(stats.message_count, 3);
        assert_eq!(stats.user_message_count, 2);
        assert_eq!(stats.assistant_message_count, 1);
        assert_eq!(stats.provider.as_deref(), Some("openai"));
        assert_eq!(stats.provider_display.as_deref(), Some("OpenAI"));
        assert_eq!(stats.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(stats.context_limit, OPENAI_CONTEXT_LIMIT);
        // No persisted usage: zeros + n/a flag, never a silent estimate.
        assert!(!stats.has_token_data);
        assert_eq!(stats.total_tokens, 0);
        assert_eq!(
            stats.total_cost_micro_usd,
            cost_micro_for_model("gpt-5.6-terra", 0, 0)
        );
        assert_eq!(stats.total_cost_micro_usd, 0);
        assert!(
            stats.usage_percent.abs() < f64::EPSILON,
            "usage must be ~0 with no persisted tokens, got {}",
            stats.usage_percent
        );
        // Sanity: the pricing path used here matches the policy rate helper.
        assert_eq!(
            cost_micro_for_model("gpt-5.6-terra", 1_000_000, 0),
            POLICY_DEFAULT_INPUT_MICRO_PER_1M
        );
    }

    #[test]
    fn tool_and_other_step_counts_aggregate_across_runs() {
        let db = in_memory_database();
        let id = seed_conversation(&db, "Agent session");
        let runs = AgentRunRepository::new(&db);
        let run_id = runs
            .create_run(Some(id), "m", "supervised")
            .expect("create run");
        runs.append_step(
            run_id,
            1,
            "model_turn",
            None,
            None,
            Some("thinking"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("model step");
        runs.append_step(
            run_id,
            2,
            "tool_call",
            Some("write_file"),
            Some("{\"path\":\"a.txt\"}"),
            Some("--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1 @@\n+x\n"),
            Some("succeeded"),
            Some(5),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("tool step");
        runs.append_step(
            run_id,
            3,
            "approval",
            Some("write_file"),
            None,
            Some("approved"),
            Some("succeeded"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("approval step");

        let stats = conversation_context_stats(&db, id).expect("stats");

        assert_eq!(stats.tool_call_count, 1);
        assert_eq!(stats.other_step_count, 2);
        assert!(!stats.has_token_data);
    }

    #[test]
    fn unknown_conversation_is_not_found() {
        let db = in_memory_database();
        let err = conversation_context_stats(&db, 9999).expect_err("missing conversation");
        assert!(matches!(err, ConversationError::NotFound { id: 9999 }));
    }

    #[test]
    fn context_limits_resolve_per_provider_and_model() {
        assert_eq!(
            context_limit_for(Some("openai"), Some("gpt-5.6-terra")),
            OPENAI_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(Some("anthropic"), Some("claude-sonnet-5")),
            ANTHROPIC_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(Some("gemini"), Some("gemini-3.6-flash")),
            GEMINI_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(Some("opencode_zen"), Some("gemini-3.6-flash")),
            GEMINI_CONTEXT_LIMIT
        );
        assert_eq!(context_limit_for(None, None), DEFAULT_CONTEXT_LIMIT);
        assert_eq!(
            context_limit_for(Some("xkiro"), Some("qwen/qwen3.5-plus:free")),
            DEFAULT_CONTEXT_LIMIT
        );
    }

    #[test]
    fn supported_model_ids_pin_to_provider_windows() {
        // NEX-AGENT-010: every ID in the provider `SUPPORTED_MODELS` lists
        // (+ the OpenAI-compatible shortlists, which ride the shared
        // executor) resolves to its provider's conservative window — read
        // from the lists themselves, so the window table cannot drift from
        // the supported-model surface by construction. The model arm owns
        // the value even when the provider is unknown/missing.
        for model in openai::SUPPORTED_MODELS {
            assert_eq!(
                context_limit_for(Some("openai"), Some(model)),
                OPENAI_CONTEXT_LIMIT,
                "openai model {model}"
            );
            assert_eq!(
                context_limit_for(None, Some(model)),
                OPENAI_CONTEXT_LIMIT,
                "openai model {model} without provider"
            );
            assert_eq!(model_context_limit(model), Some(OPENAI_CONTEXT_LIMIT));
        }
        for model in anthropic::SUPPORTED_MODELS {
            assert_eq!(
                context_limit_for(Some("anthropic"), Some(model)),
                ANTHROPIC_CONTEXT_LIMIT,
                "anthropic model {model}"
            );
            assert_eq!(
                context_limit_for(None, Some(model)),
                ANTHROPIC_CONTEXT_LIMIT,
                "anthropic model {model} without provider"
            );
            assert_eq!(model_context_limit(model), Some(ANTHROPIC_CONTEXT_LIMIT));
        }
        for model in gemini::SUPPORTED_MODELS {
            let expected = if gemini::GEMINI_LITE_MODELS.contains(model) {
                GEMINI_LITE_CONTEXT_LIMIT
            } else {
                GEMINI_CONTEXT_LIMIT
            };
            assert_eq!(
                context_limit_for(Some("gemini"), Some(model)),
                expected,
                "gemini model {model}"
            );
            assert_eq!(
                context_limit_for(None, Some(model)),
                expected,
                "gemini model {model} without provider"
            );
            assert_eq!(model_context_limit(model), Some(expected));
        }
        for model in openai::XKIRO_MODELS
            .iter()
            .chain(openai::OPENROUTER_MODELS)
            .chain(openai::NVIDIA_MODELS)
            .chain(openai::OPENCODE_ZEN_MODELS)
        {
            assert_eq!(
                model_context_limit(model),
                Some(OPENAI_CONTEXT_LIMIT),
                "compat model {model} rides the OpenAI-compatible path"
            );
        }
        // Unlisted IDs keep the old behavior: honest unknown from the model
        // arm, then the provider fallback, the `gemini`-substring rule, and
        // the default.
        assert_eq!(model_context_limit("gpt-5.6-future"), None);
        assert_eq!(model_context_limit("something-else"), None);
        assert_eq!(
            context_limit_for(Some("openai"), Some("gpt-5.6-future")),
            OPENAI_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(Some("unknown"), Some("gemini-next-1")),
            GEMINI_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(Some("unknown"), Some("something-else")),
            DEFAULT_CONTEXT_LIMIT
        );
    }

    #[test]
    fn persisted_usage_drives_totals_percent_and_cost() {
        // NEX-AGENT-005 acceptance: persist a run with per-turn usage and
        // the meter reports real totals, a positive usage percent, and the
        // policy-table cost for the resolved model.
        let db = in_memory_database();
        let provider_id = ProviderRepository::new(&db)
            .create("openai", "OpenAI")
            .expect("create provider");
        let id = seed_conversation(&db, "Metered session");
        MessageRepository::new(&db)
            .create(
                id,
                "assistant",
                "working on it",
                Some(provider_id),
                Some("gpt-5.6-terra"),
            )
            .expect("assistant message");
        let runs = AgentRunRepository::new(&db);
        let run_id = runs
            .create_run(Some(id), "gpt-5.6-terra", "supervised")
            .expect("create run");
        runs.append_step(
            run_id,
            1,
            "model_turn",
            None,
            None,
            Some("thinking"),
            None,
            None,
            None,
            None,
            None,
            Some(1_000),
            Some(500),
        )
        .expect("usage step");
        runs.append_step(
            run_id,
            2,
            "tool_call",
            Some("read_file"),
            Some("{}"),
            Some("body"),
            Some("succeeded"),
            Some(5),
            None,
            None,
            None,
            Some(2_000),
            Some(250),
        )
        .expect("second usage step");

        let stats = conversation_context_stats(&db, id).expect("stats");

        assert!(stats.has_token_data, "reported usage must be flagged");
        assert_eq!(stats.input_tokens, 3_000);
        assert_eq!(stats.output_tokens, 750);
        assert_eq!(stats.total_tokens, 3_750);
        assert!(
            stats.usage_percent > 0.0,
            "usage percent must be positive, got {}",
            stats.usage_percent
        );
        assert_eq!(
            stats.total_cost_micro_usd,
            cost_micro_for_model("gpt-5.6-terra", 3_000, 750),
            "per-conversation cost falls out of the persisted usage"
        );
        assert!(stats.total_cost_micro_usd > 0);
    }

    #[test]
    fn usage_less_turns_stay_honest_unknown() {
        // Turns whose provider omitted usage contribute nothing: the sums
        // cover only reported turns, and a conversation with no reported
        // turn at all keeps the n/a flag with zero totals.
        let db = in_memory_database();
        let id = seed_conversation(&db, "Mixed session");
        let runs = AgentRunRepository::new(&db);
        let run_id = runs
            .create_run(Some(id), "m", "supervised")
            .expect("create run");
        runs.append_step(
            run_id,
            1,
            "model_turn",
            None,
            None,
            Some("reported"),
            None,
            None,
            None,
            None,
            None,
            Some(100),
            Some(50),
        )
        .expect("usage step");
        runs.append_step(
            run_id,
            2,
            "model_turn",
            None,
            None,
            Some("unreported"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("usage-less step");

        let stats = conversation_context_stats(&db, id).expect("stats");
        assert!(stats.has_token_data);
        assert_eq!(stats.input_tokens, 100);
        assert_eq!(stats.output_tokens, 50);
        assert_eq!(stats.total_tokens, 150);

        let bare = seed_conversation(&db, "Bare session");
        let bare_run = runs
            .create_run(Some(bare), "m", "supervised")
            .expect("create run");
        runs.append_step(
            bare_run,
            1,
            "model_turn",
            None,
            None,
            Some("unreported"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("usage-less step");
        let bare_stats = conversation_context_stats(&db, bare).expect("stats");
        assert!(
            !bare_stats.has_token_data,
            "no reported turn must stay n/a, never a fabricated zero"
        );
        assert_eq!(bare_stats.total_tokens, 0);
        assert_eq!(bare_stats.total_cost_micro_usd, 0);
        assert!(bare_stats.usage_percent.abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_model_is_honest_unknown() {
        // NEX-AGENT-010: a model in no `SUPPORTED_MODELS` list resolves to
        // no per-model window (the provider fallback / default owns the
        // limit), and usage attribution still works off whatever is
        // persisted.
        assert_eq!(model_context_limit("not-a-real-model"), None);
        assert_eq!(
            context_limit_for(Some("unknown-provider"), Some("not-a-real-model")),
            DEFAULT_CONTEXT_LIMIT
        );

        let db = in_memory_database();
        let id = seed_conversation(&db, "Unknown model session");
        let runs = AgentRunRepository::new(&db);
        let run_id = runs
            .create_run(Some(id), "not-a-real-model", "supervised")
            .expect("create run");
        runs.append_step(
            run_id,
            1,
            "model_turn",
            None,
            None,
            Some("thinking"),
            None,
            None,
            None,
            None,
            None,
            Some(128),
            Some(64),
        )
        .expect("usage step");

        let stats = conversation_context_stats(&db, id).expect("stats");
        assert_eq!(stats.context_limit, DEFAULT_CONTEXT_LIMIT);
        assert!(stats.has_token_data);
        assert_eq!(stats.total_tokens, 192);
        assert!(stats.usage_percent > 0.0);
    }

    #[test]
    fn gemini_lite_clamp_matches_live_verified_window() {
        // Live-verified 2026-10-03 via the zero-token `GET
        // /v1beta/models/{modelId}` metadata endpoint (header auth, key from
        // the OS keyring, never logged): every Gemini `SUPPORTED_MODELS` ID
        // reported `inputTokenLimit = 1048576` / `outputTokenLimit = 65536`.
        // Pin the encoded values (no network in tests — the live JSON is
        // observed evidence, the consts are the commitment).
        assert_eq!(GEMINI_LITE_CONTEXT_LIMIT, 1_048_576);
        assert_eq!(GEMINI_LITE_CONTEXT_LIMIT, GEMINI_CONTEXT_LIMIT);
        for model in [
            "gemini-3.1-flash-lite",
            "gemini-flash-lite-latest",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
        ] {
            assert_eq!(
                context_limit_for(Some("gemini"), Some(model)),
                1_048_576,
                "live-verified lite model {model}"
            );
        }
    }

    #[test]
    fn mismatched_provider_model_resolves_via_model_map_first() {
        // The model arm owns the value even when the provider disagrees:
        // cross-wired combos still resolve to the model's own window.
        assert_eq!(
            context_limit_for(Some("openai"), Some("claude-sonnet-5")),
            ANTHROPIC_CONTEXT_LIMIT,
            "claude id under the openai provider"
        );
        assert_eq!(
            context_limit_for(Some("anthropic"), Some("gpt-5.6-terra")),
            OPENAI_CONTEXT_LIMIT,
            "gpt id under the anthropic provider"
        );
        assert_eq!(
            context_limit_for(Some("openai"), Some("gemini-3.6-flash")),
            GEMINI_CONTEXT_LIMIT,
            "gemini id under the openai provider"
        );
        assert_eq!(
            context_limit_for(Some("unknown-provider"), Some("gemini-3.6-flash")),
            GEMINI_CONTEXT_LIMIT,
            "known model id wins over an unknown provider"
        );
        // Unknown on both axes falls back to the default (no gemini
        // substring, no provider fallback).
        assert_eq!(
            context_limit_for(Some("unknown-provider"), Some("unknown-model")),
            DEFAULT_CONTEXT_LIMIT
        );
        assert_eq!(
            context_limit_for(None, Some("unknown-model")),
            DEFAULT_CONTEXT_LIMIT
        );
    }
}
