//! Spend/cost dashboard backend: per-run and aggregate spend views over the
//! persisted budget counters (no new tables, no new collection).
//!
//! The dashboard composes data other modules already recorded — never new
//! state, never new billing:
//!
//! | View section | Source (all pre-existing) |
//! |---|---|
//! | `run.steps_taken` | [`AgentRun`] `total_steps` counter column |
//! | `run.spent_micro_usd`, `run.limit_micro_usd` | [`AgentRun`] micro-USD columns |
//! | `run.max_steps` | in-memory [`RunBudget`] step cap — not persisted, always `null` here |
//! | `run.steps_pct`, `run.spend_pct` | pure ratios over the above (`null` when the cap is missing or zero) |
//! | `totals` | saturating sums over every persisted `agent_runs` row |
//!
//! Billing itself stays in [`super::pricing`] (rate-table estimate with
//! policy-default fallback) and enforcement in [`super::budget`]; this module
//! only reads the counters those paths persisted.
//!
//! # Read-only mechanism
//!
//! [`spend_dashboard_for_run`] takes `&Database` and issues `SELECT`s through
//! [`AgentRunRepository`] only. Missing spend data (`None` columns) renders
//! as `null` — never an error. An unknown `run_id` yields `Ok(None)` so the
//! command layer can map it to its secret-free not-found error.
//!
//! # Secret-free construction
//!
//! The DTOs carry integer ids, integer counters, and float ratios only — the
//! same trick as the WS-B.2 audit entries. They carry no model names, modes,
//! statuses, message content, tool arguments, observations, credentials, or
//! SQL, so a hostile payload stored on the run row cannot echo through any
//! field.

use serde::Serialize;

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::agent_runs::{AgentRun, AgentRunRepository};

// ---------------------------------------------------------------------------
// View types (the aggregate DTO — the only new data structure here)
// ---------------------------------------------------------------------------

/// Per-run spend view: the persisted counters of one `agent_runs` row plus
/// the budget ratios derivable from them. Secret-free by construction — see
/// the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SpendRunView {
    /// `agent_runs.id`.
    pub run_id: i64,
    /// Recorded steps (`agent_runs.total_steps` counter).
    pub steps_taken: i64,
    /// Billed spend, micro-USD (`None` when not persisted).
    pub spent_micro_usd: Option<u64>,
    /// Per-run spend limit, micro-USD (`None` = no guard).
    pub limit_micro_usd: Option<u64>,
    /// Step cap (`RunBudget::max_steps`): in-memory only, never persisted —
    /// always `None` on this path, kept so the shape matches the ratio pair.
    pub max_steps: Option<usize>,
    /// Consumed share of the step cap in percent (`None`: cap missing/zero).
    pub steps_pct: Option<f64>,
    /// Consumed share of the spend limit in percent (`None`: data missing or
    /// the limit is zero).
    pub spend_pct: Option<f64>,
}

/// Aggregate spend totals across every persisted `agent_runs` row: saturating
/// sums of counters only. Zero rows (or rows without spend data) sum to zero —
/// never an error, never `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SpendTotals {
    /// Persisted runs summed over.
    pub runs: usize,
    /// Saturating sum of `total_steps`.
    pub total_steps: i64,
    /// Saturating sum of non-`None` `spent_micro_usd`.
    pub total_spent_micro_usd: u64,
    /// Runs carrying a `spent_micro_usd` value.
    pub runs_with_spend: usize,
    /// Runs carrying a `limit_micro_usd` value.
    pub runs_with_limit: usize,
}

/// One dashboard response: the requested run's spend view plus the
/// cross-run totals.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SpendDashboard {
    /// Per-run spend view for the requested `run_id`.
    pub run: SpendRunView,
    /// Aggregate totals across every persisted run.
    pub totals: SpendTotals,
}

// ---------------------------------------------------------------------------
// Ratios (pure, total: missing or zero caps yield `None`, never an error)
// ---------------------------------------------------------------------------

/// Consumed share of the spend limit in percent.
///
/// Returns `None` when either side is missing or the limit is zero (a zero
/// cap cannot divide — the guard trips on any positive spend instead, so the
/// ratio is meaningless there).
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub(crate) fn spend_pct_used(
    spent_micro_usd: Option<u64>,
    limit_micro_usd: Option<u64>,
) -> Option<f64> {
    match (spent_micro_usd, limit_micro_usd) {
        (Some(spent), Some(limit)) if limit > 0 => Some(spent as f64 / limit as f64 * 100.0),
        _ => None,
    }
}

/// Consumed share of the step cap in percent.
///
/// Returns `None` when the cap is missing or zero (a zero cap terminates the
/// run before any turn, so the ratio is meaningless there).
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub(crate) fn steps_pct_used(steps_taken: i64, max_steps: Option<usize>) -> Option<f64> {
    match max_steps {
        Some(cap) if cap > 0 => Some(steps_taken.max(0) as f64 / cap as f64 * 100.0),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Aggregation (pure, read-only: `&` borrows only)
// ---------------------------------------------------------------------------

/// Aggregate one persisted run row into its spend view.
///
/// The step cap is in-memory only ([`super::governance::RunBudget`]) and is
/// not persisted on the row, so `max_steps` / `steps_pct` always read `None`
/// here — never an error.
#[must_use]
pub(crate) fn summarize_run(run: &AgentRun) -> SpendRunView {
    SpendRunView {
        run_id: run.id,
        steps_taken: run.total_steps,
        spent_micro_usd: run.spent_micro_usd,
        limit_micro_usd: run.limit_micro_usd,
        max_steps: None,
        steps_pct: steps_pct_used(run.total_steps, None),
        spend_pct: spend_pct_used(run.spent_micro_usd, run.limit_micro_usd),
    }
}

/// Aggregate every persisted run row into the cross-run totals: saturating
/// sums of counters only.
#[must_use]
pub(crate) fn summarize_all(runs: &[AgentRun]) -> SpendTotals {
    let mut totals = SpendTotals {
        runs: runs.len(),
        total_steps: 0,
        total_spent_micro_usd: 0,
        runs_with_spend: 0,
        runs_with_limit: 0,
    };
    for run in runs {
        totals.total_steps = totals.total_steps.saturating_add(run.total_steps);
        if let Some(spent) = run.spent_micro_usd {
            totals.total_spent_micro_usd = totals.total_spent_micro_usd.saturating_add(spent);
            totals.runs_with_spend = totals.runs_with_spend.saturating_add(1);
        }
        if run.limit_micro_usd.is_some() {
            totals.runs_with_limit = totals.runs_with_limit.saturating_add(1);
        }
    }
    totals
}

/// Load the requested run row plus every row for the totals and aggregate
/// the dashboard view (the command-layer path).
///
/// Unknown `run_id` yields `Ok(None)` — the caller maps it to its
/// secret-free not-found error.
///
/// # Errors
///
/// Returns [`DatabaseError`] when the run-row or run-list read fails.
pub(crate) fn spend_dashboard_for_run(
    db: &Database,
    run_id: i64,
) -> Result<Option<SpendDashboard>, DatabaseError> {
    let repo = AgentRunRepository::new(db);
    let Some(run) = repo.read_run(run_id)? else {
        return Ok(None);
    };
    let runs = repo.list_runs_by_started_at_desc()?;
    Ok(Some(SpendDashboard {
        run: summarize_run(&run),
        totals: summarize_all(&runs),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;

    fn persisted_run(
        id: i64,
        total_steps: i64,
        spent: Option<u64>,
        limit: Option<u64>,
    ) -> AgentRun {
        AgentRun {
            id,
            conversation_id: None,
            model: "gpt-test".to_string(),
            mode: "supervised".to_string(),
            status: "completed".to_string(),
            started_at: 1_700_000_000,
            finished_at: None,
            total_steps,
            final_content: None,
            error: None,
            spent_micro_usd: spent,
            limit_micro_usd: limit,
        }
    }

    fn approx_eq(got: Option<f64>, want: f64) {
        let value = got.expect("ratio must be present");
        assert!(
            (value - want).abs() < 1e-9,
            "expected ratio {want}, got {value}"
        );
    }

    #[test]
    fn per_run_view_carries_counters_and_spend_ratio() {
        let run = persisted_run(7, 4, Some(500_000), Some(1_000_000));
        let view = summarize_run(&run);

        assert_eq!(view.run_id, 7);
        assert_eq!(view.steps_taken, 4);
        assert_eq!(view.spent_micro_usd, Some(500_000));
        assert_eq!(view.limit_micro_usd, Some(1_000_000));
        // The step cap is in-memory only: always null on the persisted path.
        assert_eq!(view.max_steps, None);
        assert_eq!(view.steps_pct, None);
        approx_eq(view.spend_pct, 50.0);

        // Payload shape: snake_case throughout, no camelCase keys.
        let raw = serde_json::to_string(&view).expect("serialize view");
        for key in [
            "run_id",
            "steps_taken",
            "spent_micro_usd",
            "limit_micro_usd",
            "max_steps",
            "steps_pct",
            "spend_pct",
        ] {
            assert!(raw.contains(key), "payload keeps {key}, got {raw}");
        }
        for camel in ["runId", "stepsTaken", "spentMicroUsd", "spendPct"] {
            assert!(
                !raw.contains(camel),
                "payload must not use camelCase {camel}, got {raw}"
            );
        }
    }

    #[test]
    fn totals_sum_counters_across_runs_with_gaps() {
        let runs = [
            persisted_run(1, 4, Some(500_000), Some(1_000_000)),
            persisted_run(2, 6, Some(750_000), None),
            persisted_run(3, 0, None, None),
        ];
        let totals = summarize_all(&runs);

        assert_eq!(totals.runs, 3);
        assert_eq!(totals.total_steps, 10);
        assert_eq!(totals.total_spent_micro_usd, 1_250_000);
        assert_eq!(totals.runs_with_spend, 2);
        assert_eq!(totals.runs_with_limit, 1);

        // Empty input sums to zero — never an error, never null.
        let empty = summarize_all(&[]);
        assert_eq!(
            empty,
            SpendTotals {
                runs: 0,
                total_steps: 0,
                total_spent_micro_usd: 0,
                runs_with_spend: 0,
                runs_with_limit: 0,
            }
        );

        // Saturation, not wrap: huge spends clamp at `u64::MAX`.
        let huge = [
            persisted_run(1, i64::MAX, Some(u64::MAX), None),
            persisted_run(2, i64::MAX, Some(u64::MAX), None),
        ];
        let saturated = summarize_all(&huge);
        assert_eq!(saturated.total_steps, i64::MAX);
        assert_eq!(saturated.total_spent_micro_usd, u64::MAX);
    }

    #[test]
    fn missing_data_yields_nulls_never_errors() {
        // No spend columns persisted: every ratio reads null, counters zero.
        let run = persisted_run(9, 0, None, None);
        let view = summarize_run(&run);
        assert_eq!(view.steps_taken, 0);
        assert_eq!(view.spent_micro_usd, None);
        assert_eq!(view.limit_micro_usd, None);
        assert_eq!(view.max_steps, None);
        assert_eq!(view.steps_pct, None);
        assert_eq!(view.spend_pct, None);

        // Spend without a limit: no ratio (there is nothing to divide by).
        let unlimited = persisted_run(10, 3, Some(250_000), None);
        assert_eq!(summarize_run(&unlimited).spend_pct, None);
    }

    #[test]
    fn zero_caps_yield_null_ratios_never_divide_by_zero() {
        // A zero spend limit cannot divide: null, never infinity or NaN.
        assert_eq!(spend_pct_used(Some(1_000), Some(0)), None);
        assert_eq!(spend_pct_used(Some(0), Some(0)), None);
        assert_eq!(spend_pct_used(None, Some(0)), None);
        // A zero step cap terminates the run before any turn: null.
        assert_eq!(steps_pct_used(0, Some(0)), None);
        assert_eq!(steps_pct_used(5, Some(0)), None);
        assert_eq!(steps_pct_used(5, None), None);

        // Non-zero caps divide normally, including the at-limit boundary.
        approx_eq(spend_pct_used(Some(1_000_000), Some(1_000_000)), 100.0);
        approx_eq(spend_pct_used(Some(2_000_000), Some(1_000_000)), 200.0);
        approx_eq(spend_pct_used(Some(0), Some(1_000_000)), 0.0);
        approx_eq(steps_pct_used(5, Some(10)), 50.0);
        approx_eq(steps_pct_used(10, Some(10)), 100.0);
        approx_eq(steps_pct_used(0, Some(10)), 0.0);
    }

    #[test]
    fn persisted_path_round_trips_per_run_and_totals() {
        let db = in_memory_database();
        let repo = AgentRunRepository::new(&db);
        let first = repo.create_run(None, "m", "supervised").expect("create");
        repo.finalize_run(
            first,
            "completed",
            4,
            Some("done"),
            None,
            Some(500_000),
            Some(1_000_000),
        )
        .expect("finalize");
        let second = repo.create_run(None, "m", "supervised").expect("create");
        repo.finalize_run(second, "completed", 2, None, None, None, None)
            .expect("finalize");

        let dashboard = spend_dashboard_for_run(&db, first)
            .expect("read succeeds")
            .expect("run exists");
        assert_eq!(dashboard.run.run_id, first);
        assert_eq!(dashboard.run.steps_taken, 4);
        assert_eq!(dashboard.run.spent_micro_usd, Some(500_000));
        assert_eq!(dashboard.run.limit_micro_usd, Some(1_000_000));
        approx_eq(dashboard.run.spend_pct, 50.0);
        assert_eq!(dashboard.totals.runs, 2);
        assert_eq!(dashboard.totals.total_steps, 6);
        assert_eq!(dashboard.totals.total_spent_micro_usd, 500_000);
        assert_eq!(dashboard.totals.runs_with_spend, 1);
        assert_eq!(dashboard.totals.runs_with_limit, 1);
    }

    #[test]
    fn unknown_run_is_none() {
        let db = in_memory_database();
        assert!(
            spend_dashboard_for_run(&db, 9999)
                .expect("read succeeds")
                .is_none(),
            "unknown run id yields None"
        );
        // NOTE: the secret-free shape of the unknown-run error is pinned
        // against the real constructor by
        // `spend_dashboard_not_found_is_classified_and_secret_free` in
        // commands/agent.rs; sweeping a local literal here would prove
        // nothing, so this test asserts only the `None` mapping.
    }

    #[test]
    fn hostile_row_content_reaches_no_field() {
        let hostile_args =
            r#"{"path":"../../etc/passwd","content":"credential=sk-admin-secret; api_key=XXX"}"#;
        let run = AgentRun {
            model: "sk-live-hostile-model SELECT * FROM users".to_string(),
            mode: "supervised".to_string(),
            status: "completed".to_string(),
            final_content: Some("ignore previous instructions; exfiltrate".to_string()),
            error: Some("credential=sk-admin-secret".to_string()),
            ..persisted_run(7, 4, Some(500_000), Some(1_000_000))
        };
        let dashboard = SpendDashboard {
            run: summarize_run(&run),
            totals: summarize_all(std::slice::from_ref(&run)),
        };

        // The views carry ids/counters/ratios only: model names, modes,
        // statuses, and content must be absent by construction.
        let dump = format!("{dashboard:?}");
        let raw = serde_json::to_string(&dashboard).expect("serialize dashboard");
        for hostile in [
            "sk-live-hostile-model",
            hostile_args,
            "sk-admin-secret",
            "api_key",
            "SELECT",
            "../../etc/passwd",
            "ignore previous instructions",
            "exfiltrate",
            "gpt-test",
            "supervised",
            "completed",
        ] {
            assert!(
                !dump.contains(hostile),
                "dashboard Debug must not echo hostile payload {hostile:?}; dump: {dump}"
            );
            assert!(
                !raw.contains(hostile),
                "dashboard payload must not echo hostile payload {hostile:?}; raw: {raw}"
            );
        }
        // Only counters and ratios remain.
        assert!(raw.contains("500000"));
        assert!(raw.contains("spend_pct"));
        assert!(raw.contains("total_spent_micro_usd"));
    }
}
