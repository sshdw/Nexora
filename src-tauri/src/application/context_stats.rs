//! Conversation context stats: read-only per-conversation token/cost
//! breakdown (stats + diff render only; no agent behavior change).
//!
//! No new persistence: token usage is not stored per message (see
//! `application::execution::TokenUsage`, which is in-memory only and only
//! aggregated as `agent_runs.spent_micro_usd`). This service therefore reports
//! token sums as `0` with `has_token_data = false` ("n/a" in the UI) instead
//! of silently estimating. Cost is computed through the existing
//! `agent::pricing` policy so the math stays in one place.

use serde::Serialize;

use crate::application::agent::pricing;
use crate::application::conversations::{ConversationError, Result};
use crate::infrastructure::database::Database;
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
/// Fallback when the provider/model is unknown or has no committed window.
pub(crate) const DEFAULT_CONTEXT_LIMIT: u64 = 128_000;

/// Resolve the documented context limit for `provider`/`model`.
///
/// Provider names are the internal `providers.name` values (`openai`,
/// `anthropic`, `gemini`, ...); compat providers ride the OpenAI-compatible
/// path and fall back to the default. Model matching is substring-based only
/// for the `gemini` family so unknown future IDs stay honest via the default.
#[must_use]
pub(crate) fn context_limit_for(provider: Option<&str>, model: Option<&str>) -> u64 {
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
    /// Total tokens (always `0` until per-turn usage is persisted).
    pub total_tokens: u64,
    /// Input tokens (always `0`; see `has_token_data`).
    pub input_tokens: u64,
    /// Output tokens (always `0`; see `has_token_data`).
    pub output_tokens: u64,
    /// Reasoning tokens (always `0`; no persisted source).
    pub reasoning_tokens: u64,
    /// Cache-read tokens (always `0`; no persisted source).
    pub cache_read_tokens: u64,
    /// Cache-write tokens (always `0`; no persisted source).
    pub cache_write_tokens: u64,
    /// Whether any persisted token usage exists (always `false`: no
    /// `token_usage` table/columns exist, so the UI must render "n/a").
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
/// assistant message carrying attribution; tool/other step counts from the
/// conversation's persisted agent runs. Token sums are `0` with
/// `has_token_data = false` because no per-turn usage is persisted (never
/// estimated silently). Cost goes through `pricing::cost_micro_for_model`.
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

    // No persisted per-turn TokenUsage exists: report zeros with the n/a flag.
    let (input_tokens, output_tokens) = (0_u64, 0_u64);
    let total_tokens = 0_u64;
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

    let runs = AgentRunRepository::new(db).list_runs_by_conversation(conversation_id)?;
    let steps_repo = AgentRunRepository::new(db);
    let mut tool_call_count: u64 = 0;
    let mut other_step_count: u64 = 0;
    for run in &runs {
        for step in steps_repo.list_steps(run.id)? {
            if step.kind == "tool_call" {
                tool_call_count = tool_call_count.saturating_add(1);
            } else {
                other_step_count = other_step_count.saturating_add(1);
            }
        }
    }

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
        has_token_data: false,
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
}
