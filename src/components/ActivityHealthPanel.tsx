//! Activity & health panel: read-only surfaces over data the backends
//! already record — no new collection, no writes, no polling.
//!
//! ONE overlay area with two tabs (documented choice): a single rail entry
//! ("Activity & Health") opens this panel; the Activity/Health switch is an
//! M3 segmented tab group inside the panel. Two rail entries were rejected —
//! the rail foot already holds Settings/Library/VCS/Import, and a second
//! entry would split one read-only story across two destinations.
//!
//! Sources (all pre-existing query surfaces, aggregated — never duplicated):
//!   activity feed .... `activity_feed` (commands/agent.rs: batch over
//!                        persisted `agent_runs` + cross-run spend totals)
//!   conversations .... `list_conversations` (titles for feed labels)
//!   provider health .. `supported_providers` × `provider_health`
//!   budgets .......... feed `totals` + `agent.spend_limit_micro_usd` setting
//!   context pressure . `conversation_context_stats` (active conversation)
//!   git .............. `git_info` (branch + dirty-file summary)
//!   flags ............ `flags_status`
//!
//! Display rules (secret-free): feed rows render ids, fixed-vocabulary
//! labels, counters, and relative times only — never `final_content` /
//! `error` text (the backend omits those columns by construction), never
//! credentials, SQL, or message content. Times render relative
//! (`formatRelativeTime`); spend renders as compact dollars from micro-USD.
//! Manual Refresh only — no auto-refresh or live polling (documented).
//! The panel never animates on entry (instant render under reduced motion);
//! tab switches ride the segmented group's own spring, gated by motion.css.

import { useCallback, useEffect, useMemo, useState } from "react";

import { formatRelativeTime } from "../lib/format";
import {
  activityFeed,
  conversationContextStats,
  flagsStatus,
  getSetting,
  gitInfo,
  listConversations,
  providerHealth,
  supportedProviders,
  type ActivityFeed,
  type CommandError,
  type Conversation,
  type ConversationContextStats,
  type FlagsStatus,
  type GitInfo,
  type ProviderHealth,
  type SupportedProvider,
} from "../lib/tauri";
import { SPEND_LIMIT_KEY } from "../lib/useSpendLimit";
import { getLocale, tr, type Locale } from "../lib/strings";
import { useStrings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";
import M3SegmentedGroup from "./M3SegmentedGroup";

export type ActivityHealthTab = "activity" | "health";
export type ActivityKind = "all" | "runs" | "spend" | "conversations";

export interface ActivityHealthPanelProps {
  onClose: () => void;
  /** Tab to show on open (rail entry and palette deep links). */
  initialTab?: ActivityHealthTab;
  /** Palette-raised tab switch while open (token dedupes re-renders). */
  tabRequest?: { token: number; tab: ActivityHealthTab } | null;
  /** Active conversation for the context-pressure section (`null` = none). */
  activeConversationId?: number | null;
}

const FEED_LIMIT = 50;
// Server-max page for the spend filter: the backend clamps `limit` to
// 1..=100, and totals cover every persisted run, so the spend set derives
// from this wider page (the main list keeps FEED_LIMIT).
const FEED_MAX = 100;

function toMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).message === "string"
  ) {
    return (error as CommandError).message;
  }
  if (error instanceof Error) return error.message;
  return String(error);
}

/** Fixed-vocabulary run-status label; unknown values fall back to a
 * capitalized echo of the backend token (defensive — the backend only
 * sends its `agent_runs.status` vocabulary). */
function runStatusLabel(status: string, locale: Locale = getLocale()): string {
  switch (status) {
    case "running":
      return tr(locale, "activity.runRunning");
    case "completed":
      return tr(locale, "activity.runCompleted");
    case "cancelled":
      return tr(locale, "activity.runCancelled");
    case "budget_exhausted":
      return tr(locale, "activity.runBudget");
    case "spend_limit_exceeded":
      return tr(locale, "activity.runSpendHit");
    case "error":
      return tr(locale, "activity.runFailed");
    default:
      return status.charAt(0).toUpperCase() + status.slice(1);
  }
}

/** Fixed-vocabulary provider-health label. */
function healthLabel(status: ProviderHealth["status"], locale: Locale = getLocale()): string {
  switch (status) {
    case "healthy":
      return tr(locale, "activity.healthHealthy");
    case "degraded":
      return tr(locale, "activity.healthDegraded");
    case "unreachable":
      return tr(locale, "activity.healthUnreachable");
    case "unknown":
    default:
      return tr(locale, "activity.healthUnknown");
  }
}

/** Compact dollars from micro-USD (`null` = not recorded → "n/a"). */
function formatMicroUsd(micros: number | null, locale: Locale = getLocale()): string {
  if (micros === null) return tr(locale, "common.na");
  const dollars = micros / 1_000_000;
  return `$${dollars >= 1 ? dollars.toFixed(2) : dollars.toFixed(4)}`;
}

function parseSpendLimit(value: string | null): number | null {
  if (value === null) return null;
  const parsed = Number(value.trim());
  if (!Number.isInteger(parsed) || parsed <= 0) return null;
  return parsed;
}

interface FeedRow {
  key: string;
  at: number;
  kind: "run" | "conversation";
  runId?: number;
  conversationId?: number | null;
  title: string;
  detail: string;
  status?: string;
  time: number;
}

export default function ActivityHealthPanel({
  onClose,
  initialTab = "activity",
  tabRequest = null,
  activeConversationId = null,
}: ActivityHealthPanelProps) {
  const { locale, t, tp: tpn } = useStrings();
  const [tab, setTab] = useState<ActivityHealthTab>(initialTab);
  const [kind, setKind] = useState<ActivityKind>("all");
  // Palette deep links retarget the tab while open (mirrors the VCS
  // request pattern): the token identifies the request so re-renders
  // never replay it.
  const [seenToken, setSeenToken] = useState<number | null>(null);
  useEffect(() => {
    if (tabRequest !== null && tabRequest.token !== seenToken) {
      setSeenToken(tabRequest.token);
      setTab(tabRequest.tab);
    }
  }, [tabRequest, seenToken]);
  useEffect(() => {
    setTab(initialTab);
  }, [initialTab]);

  const [feed, setFeed] = useState<ActivityFeed | null>(null);
  // Wider (server-max) page backing the "spend" filter only; `null` =
  // unavailable → the filter falls back to the main `feed` page.
  const [spendFeed, setSpendFeed] = useState<ActivityFeed | null>(null);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activityLoading, setActivityLoading] = useState<boolean>(true);
  const [activityError, setActivityError] = useState<string | null>(null);

  const [supported, setSupported] = useState<SupportedProvider[]>([]);
  const [health, setHealth] = useState<Record<string, ProviderHealth>>({});
  const [healthError, setHealthError] = useState<string | null>(null);
  const [flags, setFlags] = useState<FlagsStatus | null>(null);
  const [flagsError, setFlagsError] = useState<string | null>(null);
  const [git, setGit] = useState<GitInfo | null>(null);
  const [gitError, setGitError] = useState<string | null>(null);
  const [context, setContext] = useState<ConversationContextStats | null>(null);
  const [contextError, setContextError] = useState<string | null>(null);
  const [spendLimit, setSpendLimit] = useState<number | null>(null);
  const [healthLoading, setHealthLoading] = useState<boolean>(true);

  const [refreshToken, setRefreshToken] = useState(0);

  const loadActivity = useCallback(async () => {
    setActivityLoading(true);
    setActivityError(null);
    try {
      const [feedData, convs, spendData] = await Promise.all([
        activityFeed(FEED_LIMIT),
        listConversations(),
        activityFeed(FEED_MAX).catch(() => null),
      ]);
      setFeed(feedData);
      setConversations(convs);
      setSpendFeed(spendData);
    } catch (e) {
      setActivityError(toMessage(e));
      setFeed(null);
      setSpendFeed(null);
    } finally {
      setActivityLoading(false);
    }
  }, []);

  const loadHealth = useCallback(async () => {
    setHealthLoading(true);
    setHealthError(null);
    setFlagsError(null);
    setGitError(null);
    setContextError(null);
    try {
      const defs = await supportedProviders();
      setSupported(defs);
      const probes = await Promise.all(
        defs.map(async (def) => {
          try {
            return await providerHealth(def.name);
          } catch {
            return null;
          }
        }),
      );
      const next: Record<string, ProviderHealth> = {};
      defs.forEach((def, index) => {
        const probe = probes[index];
        if (probe) next[def.name] = probe;
      });
      setHealth(next);
    } catch (e) {
      setHealthError(toMessage(e));
      setSupported([]);
    }
    try {
      setFlags(await flagsStatus());
    } catch (e) {
      setFlagsError(toMessage(e));
      setFlags(null);
    }
    try {
      setGit(await gitInfo(5));
    } catch (e) {
      setGitError(toMessage(e));
      setGit(null);
    }
    try {
      const stored = await getSetting(SPEND_LIMIT_KEY);
      setSpendLimit(parseSpendLimit(stored));
    } catch {
      setSpendLimit(null);
    }
    if (activeConversationId !== null) {
      try {
        setContext(await conversationContextStats(activeConversationId));
      } catch (e) {
        setContextError(toMessage(e));
        setContext(null);
      }
    } else {
      setContext(null);
    }
    setHealthLoading(false);
  }, [activeConversationId]);

  // Manual refresh only: the panel loads on mount / refresh click, never
  // on a timer (no auto-refresh, no live polling — documented).
  useEffect(() => {
    void loadActivity();
    void loadHealth();
  }, [loadActivity, loadHealth, refreshToken]);

  const titles = useMemo(() => {
    const map = new Map<number, string>();
    for (const conv of conversations) map.set(conv.id, conv.title);
    return map;
  }, [conversations]);

  const rows = useMemo<FeedRow[]>(() => {
    const toRunRow = (run: NonNullable<ActivityFeed["runs"]>[number]): FeedRow => {
      const convTitle =
        run.conversation_id !== null
          ? titles.get(run.conversation_id)
          : undefined;
      const parts = [
        `#${run.run_id} · ${run.model}`,
        tpn("steps", run.total_steps),
        formatMicroUsd(run.spent_micro_usd, locale),
      ];
      return {
        key: `run-${run.run_id}`,
        at: run.started_at,
        kind: "run",
        runId: run.run_id,
        conversationId: run.conversation_id,
        title:
          convTitle !== undefined ? t("activity.runIn", { title: convTitle }) : t("activity.runHash", { id: run.run_id }),
        detail: parts.join(" · "),
        status: run.status,
        time: run.started_at,
      };
    };
    if (kind === "spend") {
      // Full-history-adjacent scope: totals cover every persisted run, so
      // derive the spent set from the server-max page, not the capped
      // 50-row list page. Falls back to the main page when unavailable.
      const wideRuns = spendFeed?.runs ?? feed?.runs ?? [];
      return wideRuns
        .filter((run) => run.spent_micro_usd !== null)
        .map(toRunRow)
        .sort((a, b) => b.at - a.at);
    }
    const merged: FeedRow[] = [];
    for (const run of feed?.runs ?? []) {
      merged.push(toRunRow(run));
    }
    for (const conv of conversations) {
      merged.push({
        key: `conv-${conv.id}`,
        at: conv.created_at,
        kind: "conversation",
        conversationId: conv.id,
        title: t("activity.convCreated", { title: conv.title }),
        detail: conv.status === "archived" ? t("common.archived") : t("common.active"),
        time: conv.created_at,
      });
    }
    merged.sort((a, b) => b.at - a.at);
    if (kind === "runs") return merged.filter((row) => row.kind === "run");
    if (kind === "conversations")
      return merged.filter((row) => row.kind === "conversation");
    return merged;
  }, [feed, spendFeed, conversations, titles, kind, locale, t, tpn]);

  const flagEntries = useMemo(() => {
    if (!flags) return [];
    return Object.entries(flags.flags).sort(([a], [b]) => a.localeCompare(b));
  }, [flags]);

  return (
    <div className="nex-activity" role="group" aria-label={t("activity.group")}>
      <header className="nex-activity-header">
        <div className="nex-activity-heading">
          <h2 className="nex-activity-title">{t("activity.title")}</h2>
          <p className="nex-activity-subtitle">
            {t("activity.subtitle")}
          </p>
        </div>
        <div className="nex-activity-header-actions">
          <M3Button
            variant="quiet"
            size="sm"
            onClick={() => setRefreshToken((token) => token + 1)}
            title={t("activity.refreshTitle")}
          >
            {t("vcs.refresh")}
          </M3Button>
          <M3Button variant="quiet" size="sm" onClick={onClose}>
            {t("common.close")}
          </M3Button>
        </div>
      </header>
      <div className="nex-activity-tabs">
        <M3SegmentedGroup<ActivityHealthTab>
          label={t("activity.tabsLabel")}
          semantics="tabs"
          options={[
            { value: "activity", label: t("activity.tabActivity") },
            { value: "health", label: t("activity.tabHealth") },
          ]}
          value={tab}
          onChange={setTab}
        />
      </div>
      <div className="nex-activity-body">
        {tab === "activity" ? (
          <section aria-label={t("activity.feedAria")}>
            <div className="nex-activity-filter">
              <M3SegmentedGroup<ActivityKind>
                label={t("activity.filterLabel")}
                options={[
                  { value: "all", label: t("activity.filterAll") },
                  { value: "runs", label: t("activity.filterRuns") },
                  { value: "spend", label: t("activity.filterSpend") },
                  { value: "conversations", label: t("activity.filterChats") },
                ]}
                value={kind}
                onChange={setKind}
              />
            </div>
            {activityLoading ? (
              <M3LoadingIndicator label={t("activity.loadingActivity")} />
            ) : activityError !== null ? (
              <p className="nex-activity-error" role="alert">
                {activityError}
              </p>
            ) : rows.length === 0 ? (
              <div className="nex-activity-empty">
                <p className="nex-activity-empty-title">{t("activity.emptyTitle")}</p>
                <p className="nex-activity-empty-text">
                  {t("activity.emptyText")}
                </p>
              </div>
            ) : (
              <ul className="nex-activity-list">
                {rows.map((row) => (
                  <li key={row.key} className="nex-activity-row">
                    <div className="nex-activity-row-main">
                      <span className="nex-activity-row-title">{row.title}</span>
                      {row.status !== undefined && (
                        <span
                          className={`nex-activity-pill nex-activity-pill--${row.status}`}
                        >
                          {runStatusLabel(row.status, locale)}
                        </span>
                      )}
                    </div>
                    <div className="nex-activity-row-meta">
                      <span>{row.detail}</span>
                      <time dateTime={new Date(row.time * 1000).toISOString()}>
                        {formatRelativeTime(row.time, locale)}
                      </time>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </section>
        ) : healthLoading ? (
          <M3LoadingIndicator label={t("activity.loadingHealth")} />
        ) : (
          <>
            <section className="nex-activity-section" aria-label={t("activity.providersSection")}>
              <h3 className="nex-activity-section-title">{t("activity.providersSection")}</h3>
              {healthError !== null ? (
                <p className="nex-activity-error" role="alert">
                  {healthError}
                </p>
              ) : supported.length === 0 ? (
                <p className="nex-activity-muted">{t("activity.noProviders")}</p>
              ) : (
                <ul className="nex-activity-list">
                  {supported.map((def) => {
                    const probe = health[def.name];
                    return (
                      <li key={def.name} className="nex-activity-row">
                        <div className="nex-activity-row-main">
                          <span className="nex-activity-row-title">
                            {def.display_name}
                          </span>
                          <span className="nex-activity-pill">
                            {probe ? healthLabel(probe.status, locale) : t("activity.healthUnknown")}
                          </span>
                        </div>
                        <div className="nex-activity-row-meta">
                          <span>
                            {probe
                              ? `${probe.has_configuration ? t("activity.configured") : t("activity.notConfigured")} · ${probe.has_credential ? t("activity.keyStored") : t("activity.noKey")}`
                              : t("activity.noProbe")}
                          </span>
                          <span>{tpn("models", def.models.length)}</span>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              )}
            </section>
            <section className="nex-activity-section" aria-label={t("activity.budgetSection")}>
              <h3 className="nex-activity-section-title">{t("activity.budgetSection")}</h3>
              {feed === null ? (
                <p className="nex-activity-muted">{t("activity.spendUnavailable")}</p>
              ) : (
                <ul className="nex-activity-list">
                  <li className="nex-activity-row">
                    <div className="nex-activity-row-main">
                      <span className="nex-activity-row-title">{t("activity.allRuns")}</span>
                    </div>
                    <div className="nex-activity-row-meta">
                      <span>
                        {tpn("runs", feed.totals.runs)} · {tpn("steps", feed.totals.total_steps)} ·{" "}
                        {t("activity.totalsTotal", { total: formatMicroUsd(feed.totals.total_spent_micro_usd, locale) })}
                      </span>
                      <span>
                        {t("activity.limitValue", {
                          limit:
                            spendLimit === null
                              ? t("activity.limitNone")
                              : formatMicroUsd(spendLimit, locale),
                        })}
                      </span>
                    </div>
                  </li>
                </ul>
              )}
            </section>
            <section className="nex-activity-section" aria-label={t("activity.contextSection")}>
              <h3 className="nex-activity-section-title">{t("activity.contextSection")}</h3>
              {activeConversationId === null ? (
                <p className="nex-activity-muted">
                  {t("activity.openConv")}
                </p>
              ) : contextError !== null ? (
                <p className="nex-activity-error" role="alert">
                  {contextError}
                </p>
              ) : context === null ? (
                <p className="nex-activity-muted">{t("activity.ctxUnavailable")}</p>
              ) : (
                <div className="nex-activity-row">
                  <div className="nex-activity-row-main">
                    <span className="nex-activity-row-title">{context.title}</span>
                    <span className="nex-activity-pill">
                      {context.has_token_data
                        ? t("activity.ctxUsed", { n: context.usage_percent })
                        : t("activity.ctxNa")}
                    </span>
                  </div>
                  <div className="nex-activity-row-meta">
                    <span>
                      {t("ctx.labelMessages")}: {context.message_count} ·{" "}
                      {t("ctx.legendTools")}: {context.tool_call_count}
                    </span>
                    <span>{formatMicroUsd(context.total_cost_micro_usd, locale)}</span>
                  </div>
                  {context.has_token_data && (
                    <div
                      className="nex-activity-meter"
                      role="progressbar"
                      aria-valuenow={context.usage_percent}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-label={t("activity.ctxAria")}
                    >
                      <div
                        className="nex-activity-meter-fill"
                        style={{ width: `${Math.min(100, context.usage_percent)}%` }}
                      />
                    </div>
                  )}
                </div>
              )}
            </section>
            <section className="nex-activity-section" aria-label={t("activity.gitSection")}>
              <h3 className="nex-activity-section-title">{t("activity.gitSection")}</h3>
              {gitError !== null ? (
                <p className="nex-activity-muted">{gitError}</p>
              ) : git === null ? (
                <p className="nex-activity-muted">{t("activity.gitUnavailable")}</p>
              ) : (
                <div className="nex-activity-row">
                  <div className="nex-activity-row-main">
                    <span className="nex-activity-row-title">
                      {git.branch ?? t("vcs.detachedHead")}
                    </span>
                    <span className="nex-activity-pill">
                      {git.files.length === 0 ? t("activity.clean") : t("activity.dirty")}
                    </span>
                  </div>
                  <div className="nex-activity-row-meta">
                    <span>
                      {git.files.length === 0
                        ? t("activity.noChanged")
                        : `${tpn("files", git.files.length)}${
                            git.files_overflow > 0 ? ` ${t("activity.filesMore", { n: git.files_overflow })}` : ""
                          }`}
                    </span>
                    <span>
                      {git.commits.length === 0
                        ? t("activity.noCommits")
                        : tpn("commits", git.commits.length)}
                    </span>
                  </div>
                </div>
              )}
            </section>
            <section className="nex-activity-section" aria-label={t("activity.flagsSection")}>
              <h3 className="nex-activity-section-title">{t("activity.flagsSection")}</h3>
              {flagsError !== null ? (
                <p className="nex-activity-error" role="alert">
                  {flagsError}
                </p>
              ) : flagEntries.length === 0 ? (
                <p className="nex-activity-muted">{t("activity.noFlags")}</p>
              ) : (
                <ul className="nex-activity-list">
                  {flagEntries.map(([name, flag]) => (
                    <li key={name} className="nex-activity-row">
                      <div className="nex-activity-row-main">
                        <span className="nex-activity-row-title">{name}</span>
                        <span className="nex-activity-pill">
                          {flag.enabled ? t("common.on") : t("common.off")}
                        </span>
                      </div>
                      <div className="nex-activity-row-meta">
                        <span>{t("activity.sourcePrefix", { source: flag.source })}</span>
                        <span>{flag.enforced ? t("activity.enforced") : t("activity.notEnforced")}</span>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          </>
        )}
      </div>
    </div>
  );
}
