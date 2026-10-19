//! Context panel — a MODE, not furniture (MAP row 20).
//!
//! Presentational over the existing IPC wrappers: stats come from the
//! `conversation_context_stats` command; diffs are grouped client-side from
//! the already-persisted `write_file` step observations (`list_agent_runs` /
//! `list_agent_steps`). Diff rendering reuses the existing `DiffView`
//! classifier from `AgentRunSteps` — no new diff engine.
//!
//! #230 P3 — three changes, all contract-driven:
//!
//!  1. BUDGET HAIRLINE + READOUT (MAP row 20, `progress.md`). The panel's
//!     one control is the budget slider, and its value is real rather than
//!     decorative: it is persisted (src/lib/useContextBudget.ts) and the
//!     reading under it — tokens used against tokens budgeted — is computed
//!     from that same number, so moving the thumb changes what the panel
//!     says. The hairline is that reading drawn as a proportion: a filled
//!     track at the used share, never a generic spinner and never a bar that
//!     animates on its own. Its accessible name carries the two numbers, so
//!     the proportion is never visual-only.
//!
//!  2. COST ON HOVER (SYSTEM §8 "cost on hover … details without permanent
//!     clutter"). Total cost was a permanent label/value cell among sixteen
//!     others. It is now the budget readout's `title` — one hover away, not
//!     a permanent row — and the SAME sentence is in the hairline's
//!     accessible name, so the detail is never hover-only for a keyboard or
//!     screen-reader user. The slider's own description is left exactly as
//!     it was: it describes the budget, and the cost is not part of what the
//!     budget IS.
//!
//!  3. THE BREAKDOWN LEGEND IS NO LONGER HUE (THEME §1). The four segments
//!     are CATEGORICAL data (who spent the tokens), not state, so they may
//!     not be painted as if they were. They now read as four tone steps off
//!     the achromatic ink ladder (see context.css), which survives grayscale
//!     and high-contrast, and each legend entry keeps its text label and its
//!     percentage — the meaning never rides on colour alone (SYSTEM §8).

import { useEffect, useId, useState } from "react";

import {
  conversationContextStats,
  getAgentStepObservation,
  listAgentRuns,
  listAgentSteps,
  type AgentStep,
  type ConversationContextStats,
} from "../lib/tauri";
import { DiffView } from "./AgentRunSteps";
import M3LoadingIndicator from "./M3LoadingIndicator";
import {
  CONTEXT_BUDGET_MAX,
  CONTEXT_BUDGET_MIN,
  CONTEXT_BUDGET_STEP,
  useContextBudget,
} from "../lib/useContextBudget";
import { getLocale, tr, type Locale } from "../lib/strings";
import { useStrings } from "../lib/useLocale";
import { Slider } from "./ui/slider";

export interface ChangedFile {
  path: string;
  observation: string;
  runId: number;
  seq: number;
}

function parseWritePath(args: string | null): string | null {
  if (!args) return null;
  try {
    const parsed = JSON.parse(args) as Record<string, unknown>;
    const path = parsed.path;
    return typeof path === "string" && path.length > 0 ? path : null;
  } catch {
    return null;
  }
}

function formatCount(value: number): string {
  return value.toLocaleString("en-US").replace(/,/g, " ");
}

function formatTokens(value: number, known: boolean, locale: Locale = getLocale()): string {
  if (!known) return tr(locale, "common.na");
  return formatCount(value);
}

function formatCost(microUsd: number): string {
  const usd = microUsd / 1_000_000;
  return `${usd.toFixed(2)} $`;
}

function formatDateTime(ts: number): string {
  const d = new Date(ts * 1000);
  return d.toLocaleString("en-GB", {
    day: "2-digit",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export default function ContextPanel({ conversationId }: { conversationId: number }) {
  const { t } = useStrings();
  const [stats, setStats] = useState<ConversationContextStats | null>(null);
  const [files, setFiles] = useState<ChangedFile[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  // #220: the budget slider's own state (persisted, clamped) — read
  // by the panel's usage reading below, so the control is wired to
  // something real rather than sitting there looking adjustable.
  const budgetStore = useContextBudget();
  const hintId = useId();

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    setStats(null);
    setFiles([]);

    const load = async () => {
      try {
        const data = await conversationContextStats(conversationId);
        if (cancelled) return;
        setStats(data);
        const runs = await listAgentRuns(conversationId);
        if (cancelled) return;
        // First pass over previews only: elect the winning step per path
        // (max seq, same rule as before) so the full-text fetch below hits
        // ONLY winners instead of every write_file step sequentially.
        const winners = new Map<
          string,
          { path: string; preview: string; runId: number; seq: number; stepId: number }
        >();
        for (const run of runs) {
          let steps: AgentStep[] = [];
          try {
            steps = await listAgentSteps(run.id);
          } catch {
            steps = [];
          }
          for (const step of steps) {
            if (step.kind !== "tool_call" || step.tool_name !== "write_file") continue;
            if (!step.observation) continue;
            const path = parseWritePath(step.arguments) ?? tr(getLocale(), "ctx.runStepFallback", { run: run.id, seq: step.seq });
            const prev = winners.get(path);
            if (!prev || step.seq >= prev.seq) {
              winners.set(path, {
                path,
                preview: step.observation,
                runId: run.id,
                seq: step.seq,
                stepId: step.id,
              });
            }
          }
        }
        if (cancelled) return;
        // NEX-PERF-003: list items carry bounded previews; the file view
        // needs the full diff, fetched on demand per winning write_file step
        // (best-effort — a failed fetch still renders the preview, whose
        // marker says honestly that it was cut).
        const files = await Promise.all(
          [...winners.values()].map(async (win) => {
            let observation = win.preview;
            try {
              const full = await getAgentStepObservation(win.stepId);
              if (cancelled) return { ...win, observation: win.preview };
              observation = full;
            } catch {
              // Keep the preview fallback.
            }
            return { ...win, observation };
          }),
        );
        if (cancelled) return;
        setFiles(
          files
            .map(({ path, observation, runId, seq }) => ({ path, observation, runId, seq }))
            .sort((a, b) => a.path.localeCompare(b.path)),
        );
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    void load();
    return () => {
      cancelled = true;
    };
  }, [conversationId]);

  if (loading) {
    return <M3LoadingIndicator label={t("ctx.loading")} />;
  }

  if (error || !stats) {
    return (
      <div className="nex-composer-error nex-fade-in" role="alert">
        {error ?? t("ctx.unavailable")}
      </div>
    );
  }

  const user = stats.user_message_count;
  const assistant = stats.assistant_message_count;
  const tools = stats.tool_call_count;
  const other = stats.other_step_count;
  const total = user + assistant + tools + other;
  const pct = (v: number): number => (total === 0 ? 0 : (v / total) * 100);

  // Budget reading (#220): what the user budgeted against what the
  // conversation has actually used. Distinct from the `usage_percent`
  // row above, which measures against the model's own context limit —
  // this one moves when the user moves the slider.
  const budget = budgetStore.budget;
  const budgetUsed = stats.has_token_data ? stats.total_tokens : null;
  // `>` and not `>=`: landing exactly ON the budget is using it all,
  // not exceeding it. No token data means no verdict either way.
  const overBudget = budgetUsed !== null && budgetUsed > budget;
  // The hairline's fill share, clamped to the track: a bar never draws
  // past its own end, and the moment there is no reading to show it stays
  // EMPTY rather than animating or guessing a value (SYSTEM §8, no fake
  // progress). Over budget it pins at 100% and the readout below — not the
  // bar — carries the over-budget verdict.
  const budgetShare =
    budgetUsed === null || budget <= 0 ? 0 : Math.min(100, (budgetUsed / budget) * 100);

  const rows: Array<{ label: string; value: string; title?: string }> = [
    { label: t("ctx.labelSession"), value: stats.title },
    { label: t("ctx.labelMessages"), value: formatCount(stats.message_count) },
    {
      label: t("ctx.labelProvider"),
      value: stats.provider_display ?? stats.provider ?? t("common.na"),
    },
    { label: t("ctx.labelModel"), value: stats.model ?? t("common.na") },
    { label: t("ctx.labelContextLimit"), value: formatCount(stats.context_limit) },
    { label: t("ctx.labelTotalTokens"), value: formatTokens(stats.total_tokens, stats.has_token_data) },
    {
      label: t("ctx.labelUsage"),
      value: `${stats.usage_percent.toFixed(0)}%`,
    },
    {
      label: t("ctx.labelInputTokens"),
      value: formatTokens(stats.input_tokens, stats.has_token_data),
      title: stats.has_token_data ? undefined : t("ctx.noTokenData"),
    },
    {
      label: t("ctx.labelOutputTokens"),
      value: formatTokens(stats.output_tokens, stats.has_token_data),
      title: stats.has_token_data ? undefined : t("ctx.noTokenData"),
    },
    {
      label: t("ctx.labelReasoningTokens"),
      value: formatTokens(stats.reasoning_tokens, stats.has_token_data),
      title: stats.has_token_data ? undefined : t("ctx.noTokenData"),
    },
    {
      label: t("ctx.labelCacheTokens"),
      value:
        stats.has_token_data
          ? `${formatCount(stats.cache_read_tokens)} / ${formatCount(stats.cache_write_tokens)}`
          : t("common.na"),
      title: stats.has_token_data ? undefined : t("ctx.noTokenData"),
    },
    { label: t("ctx.labelUserMessages"), value: formatCount(stats.user_message_count) },
    { label: t("ctx.labelAssistantMessages"), value: formatCount(stats.assistant_message_count) },
    { label: t("ctx.labelSessionCreated"), value: formatDateTime(stats.created_at) },
    { label: t("ctx.labelLastActivity"), value: formatDateTime(stats.updated_at) },
  ];
  // Cost rides on the budget readout and the hairline's accessible name,
  // not on a sixteenth permanent row (SYSTEM §8: "Cost on hover … details
  // without permanent clutter", and never hover-ONLY).
  const cost = formatCost(stats.total_cost_micro_usd);

  return (
    <div className="nex-context nex-view-enter" role="group" aria-label={t("ctx.group")}>
      <dl className="nex-context-grid">
        {rows.map((row) => (
          <div key={row.label} className="nex-context-cell">
            <dt className="nex-context-label">{row.label}</dt>
            <dd className="nex-context-value" title={row.title}>
              {row.value}
            </dd>
          </div>
        ))}
      </dl>

      <section className="nex-context-breakdown" aria-label={t("ctx.breakdown")}>
        <h3 className="nex-context-section-title">{t("ctx.breakdown")}</h3>
        <div
          className="nex-context-bar"
          role="img"
          aria-label={t("ctx.breakdownAria", {
            u: pct(user).toFixed(1),
            a: pct(assistant).toFixed(1),
            t: pct(tools).toFixed(1),
            o: pct(other).toFixed(1),
          })}
        >
          <span
            className="nex-context-seg nex-context-seg-user"
            style={{ width: `${pct(user)}%` }}
          />
          <span
            className="nex-context-seg nex-context-seg-assistant"
            style={{ width: `${pct(assistant)}%` }}
          />
          <span
            className="nex-context-seg nex-context-seg-tools"
            style={{ width: `${pct(tools)}%` }}
          />
          <span
            className="nex-context-seg nex-context-seg-other"
            style={{ width: `${pct(other)}%` }}
          />
        </div>
        <ul className="nex-context-legend">
          <li>
            <span className="nex-context-dot nex-context-seg-user" aria-hidden="true" />
            {t("ctx.legendUser")} {pct(user).toFixed(1)}%
          </li>
          <li>
            <span className="nex-context-dot nex-context-seg-assistant" aria-hidden="true" />
            {t("ctx.legendAssistant")} {pct(assistant).toFixed(1)}%
          </li>
          <li>
            <span className="nex-context-dot nex-context-seg-tools" aria-hidden="true" />
            {t("ctx.legendTools")} {pct(tools).toFixed(1)}%
          </li>
          <li>
            <span className="nex-context-dot nex-context-seg-other" aria-hidden="true" />
            {t("ctx.legendOther")} {pct(other).toFixed(1)}%
          </li>
        </ul>
      </section>

      <section className="nex-context-budget" aria-labelledby={`${hintId}-title`}>
        <h3 className="nex-context-section-title" id={`${hintId}-title`}>
          {t("ctx.budgetTitle")}
        </h3>
        <div className="nex-context-budget-control">
          <Slider
            value={[budget]}
            min={CONTEXT_BUDGET_MIN}
            max={CONTEXT_BUDGET_MAX}
            step={CONTEXT_BUDGET_STEP}
            // On the THUMB, where `role="slider"` actually lives.
            thumbProps={{ "aria-label": t("ctx.budgetLabel"), "aria-describedby": hintId }}
            // Live while dragging; persisted only on release, so a
            // drag across the track is one write, not two hundred.
            onValueChange={(next) => budgetStore.setBudget(next[0])}
            onValueCommit={(next) => budgetStore.commitBudget(next[0])}
          />
          <span className="nex-context-budget-value">
            {t("ctx.budgetTokens", { n: formatCount(budget) })}
          </span>
        </div>
        {/* The budget HAIRLINE (MAP row 20, `progress.md`): the readout
            above drawn as a proportion. One filled track, no labels
            inside it, no self-animation — it is a static measurement of a
            persisted number that only changes when the number changes.
            The numbers live in the accessible name AND in the line below,
            so the proportion is never visual-only. */}
        <div
          className={
            "nex-context-meter" +
            (overBudget ? " is-over" : "") +
            (budgetUsed === null ? " is-empty" : "")
          }
          role="img"
          /* Both numbers AND the cost, in the element's accessible name —
             which is what keeps cost-on-hover from being hover-ONLY
             (SYSTEM §8). The visible `title` on the reading below is the
             mouse affordance for the same sentence. */
          aria-label={
            (budgetUsed === null
              ? t("ctx.noTokenData")
              : t("ctx.budgetMeterAria", {
                  used: formatCount(budgetUsed),
                  budget: formatCount(budget),
                })) +
            ". " +
            t("ctx.totalCostTitle", { cost })
          }
        >
          <span className="nex-context-meter-fill" style={{ width: `${budgetShare}%` }} />
        </div>
        {/* The live reading. Over budget it is announced as a status
            (the value changed under a control that was already there)
            and it takes the destructive tone — which is what the hint
            below promises, so the hint has to be true or be deleted.
            Cost rides on this line's `title` (MAP row 20, "cost-on-hover")
            and in the hairline's accessible name above, so the detail is
            never hover-only. */}
        <p
          className={
            "nex-context-budget-reading" + (overBudget ? " is-over" : "")
          }
          role={overBudget ? "status" : undefined}
          title={t("ctx.totalCostTitle", { cost })}
        >
          {budgetUsed === null
            ? t("ctx.noTokenData")
            : overBudget
              ? t("ctx.budgetExceeded", { budget: formatCount(budget) })
              : t("ctx.budgetUsed", {
                  used: formatCount(budgetUsed),
                  budget: formatCount(budget),
                })}
        </p>
        <p className="nex-context-budget-hint" id={hintId}>
          {t("ctx.budgetHint")}
        </p>
        {budgetStore.error && (
          <p className="nex-context-budget-error" role="alert">
            {budgetStore.error}
          </p>
        )}
      </section>

      <section className="nex-context-files" aria-label={t("vcs.changedTitle", { n: files.length })}>
        <h3 className="nex-context-section-title">
          {t("vcs.changedTitle", { n: files.length })}
        </h3>
        {files.length === 0 ? (
          <p className="nex-agent-empty">{t("ctx.noFiles")}</p>
        ) : (
          <ul className="nex-context-file-list">
            {files.map((file) => (
              <li key={`${file.runId}-${file.seq}-${file.path}`} className="nex-context-file">
                <header className="nex-context-file-header">
                  <span className="nex-tag nex-tag-mono" title={t("ctx.changedPath")}>
                    {file.path}
                  </span>
                </header>
                <DiffView observation={file.observation} />
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
