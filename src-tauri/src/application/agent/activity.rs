//! Activity feed backend: one read-only aggregate over the persisted run
//! history plus cross-run spend totals (no new tables, no new collection).
//!
//! The feed composes data other paths already recorded — never new state:
//!
//! | View section | Source (all pre-existing) |
//! |---|---|
//! | `runs[]` | [`AgentRun`] rows, `started_at` DESC, capped server-side |
//! | `totals` | [`super::spend::summarize_all`] over every persisted row |
//!
//! # Why a batch command
//!
//! The repository already lists every run by recency
//! ([`AgentRunRepository::list_runs_by_started_at_desc`]), but no IPC command
//! exposed it: the frontend could only fan out `list_agent_runs` once per
//! conversation (N+1 round trips) and could only reach the cross-run spend
//! totals through `spend_dashboard`, which needs an anchor `run_id` and fails
//! when zero runs exist. This module merges those two pre-existing reads into
//! one round trip behind the single additive `activity_feed` command.
//!
//! # Read-only mechanism
//!
//! [`activity_feed_for`] takes `&Database` and issues one `SELECT` through
//! [`AgentRunRepository`] only. The totals are derived in memory from the same
//! row set, so the feed and the totals can never disagree.
//!
//! # Secret-free construction
//!
//! [`ActivityRun`] carries integer ids, fixed-vocabulary labels (`model`,
//! `mode`, `status` — the same values `list_agent_runs` already exposes),
//! counters, and Unix-seconds timestamps only. It deliberately omits the
//! row's `final_content` and `error` columns: those carry model output and
//! provider error text, so feed rows render metadata + labels and never
//! content snippets. A hostile payload stored on the run row cannot echo
//! through any field (the same trick as the WS-B.2 audit entries).

use serde::Serialize;

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::agent_runs::{AgentRun, AgentRunRepository};

use super::spend::{summarize_all, SpendTotals};

/// Default entries when the caller passes no limit.
const DEFAULT_FEED_ENTRIES: u32 = 50;

/// Upper bound for the `limit` argument (clamped, never an error).
const MAX_FEED_ENTRIES: u32 = 100;

/// One feed row: the metadata of a persisted `agent_runs` row, newest first.
/// Secret-free by construction — see the module docs: no `final_content`,
/// no `error`, no arguments, no observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ActivityRun {
    /// `agent_runs.id`.
    pub run_id: i64,
    /// Owning conversation (`None` for pre-wiring rows).
    pub conversation_id: Option<i64>,
    /// Provider model name for the run (label only, never a credential).
    pub model: String,
    /// Autonomy mode at run start (fixed vocabulary).
    pub mode: String,
    /// Run state (fixed vocabulary: `running` / `completed` / ... / `error`).
    pub status: String,
    /// Start timestamp (Unix seconds).
    pub started_at: i64,
    /// Termination timestamp (Unix seconds, `None` while active).
    pub finished_at: Option<i64>,
    /// Recorded steps (`total_steps` counter).
    pub total_steps: i64,
    /// Billed spend, micro-USD (`None` when not persisted).
    pub spent_micro_usd: Option<u64>,
    /// Per-run spend limit, micro-USD (`None` = no guard).
    pub limit_micro_usd: Option<u64>,
}

impl From<&AgentRun> for ActivityRun {
    fn from(run: &AgentRun) -> Self {
        Self {
            run_id: run.id,
            conversation_id: run.conversation_id,
            model: run.model.clone(),
            mode: run.mode.clone(),
            status: run.status.clone(),
            started_at: run.started_at,
            finished_at: run.finished_at,
            total_steps: run.total_steps,
            spent_micro_usd: run.spent_micro_usd,
            limit_micro_usd: run.limit_micro_usd,
        }
    }
}

/// One feed response: the capped recent-run rows plus the cross-run totals
/// over every persisted run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ActivityFeed {
    /// Recent runs, `started_at` DESC, capped at the clamped limit.
    pub runs: Vec<ActivityRun>,
    /// Aggregate totals across every persisted run.
    pub totals: SpendTotals,
}

/// Load the activity feed: every persisted run row (one `SELECT`,
/// `started_at` DESC) mapped to secret-free rows capped at the clamped limit,
/// plus the cross-run spend totals over the full row set.
///
/// `limit` clamps to `1..=MAX_FEED_ENTRIES` (`None` = default); clamping is
/// never an error.
///
/// # Errors
///
/// Returns [`DatabaseError`] when the run-list read fails.
pub(crate) fn activity_feed_for(
    db: &Database,
    limit: Option<u32>,
) -> Result<ActivityFeed, DatabaseError> {
    let count = limit
        .unwrap_or(DEFAULT_FEED_ENTRIES)
        .clamp(1, MAX_FEED_ENTRIES) as usize;
    let all = AgentRunRepository::new(db).list_runs_by_started_at_desc()?;
    let totals = summarize_all(&all);
    let runs = all.iter().take(count).map(ActivityRun::from).collect();
    Ok(ActivityFeed { runs, totals })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;

    fn seed_run(
        db: &Database,
        model: &str,
        status: &str,
        steps: i64,
        spent: Option<u64>,
        spent_limit: Option<u64>,
    ) -> i64 {
        let repo = AgentRunRepository::new(db);
        let id = repo
            .create_run(None, model, "supervised")
            .expect("seed run");
        repo.finalize_run(id, status, steps, Some("done"), None, spent, spent_limit)
            .expect("finalize seed run");
        id
    }

    #[test]
    fn empty_database_feeds_empty_rows_and_zero_totals() {
        let db = in_memory_database();
        let feed = activity_feed_for(&db, None).expect("feed reads");
        assert!(feed.runs.is_empty());
        assert_eq!(feed.totals.runs, 0);
        assert_eq!(feed.totals.total_steps, 0);
        assert_eq!(feed.totals.total_spent_micro_usd, 0);
    }

    #[test]
    fn feed_orders_newest_first_and_sums_totals() {
        let db = in_memory_database();
        let first = seed_run(&db, "model-a", "completed", 3, Some(100), Some(1_000));
        let second = seed_run(&db, "model-b", "completed", 5, Some(200), None);
        let feed = activity_feed_for(&db, None).expect("feed reads");
        assert_eq!(feed.runs.len(), 2);
        // Same-second seeds: DESC is non-strict, but both rows appear once.
        let ids: Vec<i64> = feed.runs.iter().map(|row| row.run_id).collect();
        assert!(ids.contains(&first) && ids.contains(&second));
        assert_eq!(feed.totals.runs, 2);
        assert_eq!(feed.totals.total_steps, 8);
        assert_eq!(feed.totals.total_spent_micro_usd, 300);
        assert_eq!(feed.totals.runs_with_spend, 2);
        assert_eq!(feed.totals.runs_with_limit, 1);
    }

    #[test]
    fn limit_clamps_instead_of_erroring() {
        let db = in_memory_database();
        for index in 0..3 {
            seed_run(
                &db,
                format!("model-{index}").as_str(),
                "completed",
                1,
                None,
                None,
            );
        }
        // Zero clamps to one row.
        let one = activity_feed_for(&db, Some(0)).expect("zero clamps");
        assert_eq!(one.runs.len(), 1);
        // Totals still cover every persisted run, not just the capped page.
        assert_eq!(one.totals.runs, 3);
        // Huge limits clamp without error and return every row.
        let all = activity_feed_for(&db, Some(10_000)).expect("huge clamps");
        assert_eq!(all.runs.len(), 3);
    }

    #[test]
    fn feed_rows_never_carry_content_or_error_text() {
        let db = in_memory_database();
        let repo = AgentRunRepository::new(&db);
        let id = repo
            .create_run(None, "model-x", "supervised")
            .expect("seed run");
        repo.finalize_run(
            id,
            "error",
            1,
            Some("HOSTILE final content sk-secret"),
            Some("HOSTILE error text credential"),
            None,
            None,
        )
        .expect("finalize seed run");
        let feed = activity_feed_for(&db, None).expect("feed reads");
        let rendered = serde_json::to_string(&feed).expect("feed serializes");
        assert!(
            !rendered.contains("HOSTILE"),
            "run content must not echo through the feed: {rendered}"
        );
        assert!(!rendered.contains("final_content"));
        assert!(rendered.contains("\"status\":\"error\""));
    }
}
