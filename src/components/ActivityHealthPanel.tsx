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
function runStatusLabel(status: string): string {
  switch (status) {
    case "running":
      return "Running";
    case "completed":
      return "Completed";
    case "cancelled":
      return "Cancelled";
    case "budget_exhausted":
      return "Budget exhausted";
    case "spend_limit_exceeded":
      return "Spend limit hit";
    case "error":
      return "Failed";
    default:
      return status.charAt(0).toUpperCase() + status.slice(1);
  }
}

/** Fixed-vocabulary provider-health label. */
function healthLabel(status: ProviderHealth["status"]): string {
  switch (status) {
    case "healthy":
      return "Healthy";
    case "degraded":
      return "Degraded";
    case "unreachable":
      return "Unreachable";
    case "unknown":
    default:
      return "Unknown";
  }
}

/** Compact dollars from micro-USD (`null` = not recorded → "n/a"). */
function formatMicroUsd(micros: number | null): string {
  if (micros === null) return "n/a";
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
      const [feedData, convs] = await Promise.all([
        activityFeed(FEED_LIMIT),
        listConversations(),
      ]);
      setFeed(feedData);
      setConversations(convs);
    } catch (e) {
      setActivityError(toMessage(e));
      setFeed(null);
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
    const merged: FeedRow[] = [];
    for (const run of feed?.runs ?? []) {
      const convTitle =
        run.conversation_id !== null
          ? titles.get(run.conversation_id)
          : undefined;
      const parts = [
        `#${run.run_id} · ${run.model}`,
        `${run.total_steps} steps`,
        formatMicroUsd(run.spent_micro_usd),
      ];
      merged.push({
        key: `run-${run.run_id}`,
        at: run.started_at,
        kind: "run",
        runId: run.run_id,
        conversationId: run.conversation_id,
        title:
          convTitle !== undefined ? `Run in “${convTitle}”` : `Run #${run.run_id}`,
        detail: parts.join(" · "),
        status: run.status,
        time: run.started_at,
      });
    }
    for (const conv of conversations) {
      merged.push({
        key: `conv-${conv.id}`,
        at: conv.created_at,
        kind: "conversation",
        conversationId: conv.id,
        title: `Conversation “${conv.title}” created`,
        detail: conv.status === "archived" ? "Archived" : "Active",
        time: conv.created_at,
      });
    }
    merged.sort((a, b) => b.at - a.at);
    if (kind === "runs") return merged.filter((row) => row.kind === "run");
    if (kind === "conversations")
      return merged.filter((row) => row.kind === "conversation");
    if (kind === "spend") {
      const spentById = new Map(
        (feed?.runs ?? [])
          .filter((run) => run.spent_micro_usd !== null)
          .map((run) => [run.run_id, true]),
      );
      return merged.filter((row) => row.runId !== undefined && spentById.has(row.runId));
    }
    return merged;
  }, [feed, conversations, titles, kind]);

  const flagEntries = useMemo(() => {
    if (!flags) return [];
    return Object.entries(flags.flags).sort(([a], [b]) => a.localeCompare(b));
  }, [flags]);

  return (
    <div className="nex-activity" aria-label="Activity and health">
      <header className="nex-activity-header">
        <div className="nex-activity-heading">
          <h2 className="nex-activity-title">Activity &amp; Health</h2>
          <p className="nex-activity-subtitle">
            Recent runs, spend, and project state — refresh manually.
          </p>
        </div>
        <div className="nex-activity-header-actions">
          <M3Button
            variant="quiet"
            size="sm"
            onClick={() => setRefreshToken((token) => token + 1)}
            title="Reload activity and health from the backend"
          >
            Refresh
          </M3Button>
          <M3Button variant="quiet" size="sm" onClick={onClose}>
            Close
          </M3Button>
        </div>
      </header>
      <div className="nex-activity-tabs">
        <M3SegmentedGroup<ActivityHealthTab>
          label="Activity and health views"
          semantics="tabs"
          options={[
            { value: "activity", label: "Activity" },
            { value: "health", label: "Health" },
          ]}
          value={tab}
          onChange={setTab}
        />
      </div>
      <div className="nex-activity-body">
        {tab === "activity" ? (
          <section aria-label="Activity feed">
            <div className="nex-activity-filter">
              <M3SegmentedGroup<ActivityKind>
                label="Filter feed by kind"
                options={[
                  { value: "all", label: "All" },
                  { value: "runs", label: "Runs" },
                  { value: "spend", label: "Spend" },
                  { value: "conversations", label: "Chats" },
                ]}
                value={kind}
                onChange={setKind}
              />
            </div>
            {activityLoading ? (
              <M3LoadingIndicator label="Loading activity" />
            ) : activityError !== null ? (
              <p className="nex-activity-error" role="alert">
                {activityError}
              </p>
            ) : rows.length === 0 ? (
              <div className="nex-activity-empty">
                <p className="nex-activity-empty-title">No activity yet</p>
                <p className="nex-activity-empty-text">
                  Conversations and agent runs will appear here as they happen.
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
                          {runStatusLabel(row.status)}
                        </span>
                      )}
                    </div>
                    <div className="nex-activity-row-meta">
                      <span>{row.detail}</span>
                      <time dateTime={new Date(row.time * 1000).toISOString()}>
                        {formatRelativeTime(row.time)}
                      </time>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </section>
        ) : healthLoading ? (
          <M3LoadingIndicator label="Loading project health" />
        ) : (
          <>
            <section className="nex-activity-section" aria-label="Providers">
              <h3 className="nex-activity-section-title">Providers</h3>
              {healthError !== null ? (
                <p className="nex-activity-error" role="alert">
                  {healthError}
                </p>
              ) : supported.length === 0 ? (
                <p className="nex-activity-muted">No providers listed.</p>
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
                            {probe ? healthLabel(probe.status) : "Unknown"}
                          </span>
                        </div>
                        <div className="nex-activity-row-meta">
                          <span>
                            {probe
                              ? `${probe.has_configuration ? "configured" : "not configured"} · ${probe.has_credential ? "key stored" : "no key"}`
                              : "No health probe"}
                          </span>
                          <span>{def.models.length} models</span>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              )}
            </section>
            <section className="nex-activity-section" aria-label="Budget and spend">
              <h3 className="nex-activity-section-title">Budget &amp; spend</h3>
              {feed === null ? (
                <p className="nex-activity-muted">Spend totals unavailable.</p>
              ) : (
                <ul className="nex-activity-list">
                  <li className="nex-activity-row">
                    <div className="nex-activity-row-main">
                      <span className="nex-activity-row-title">All runs</span>
                    </div>
                    <div className="nex-activity-row-meta">
                      <span>
                        {feed.totals.runs} runs · {feed.totals.total_steps} steps ·{" "}
                        {formatMicroUsd(feed.totals.total_spent_micro_usd)} total
                      </span>
                      <span>
                        Limit:{" "}
                        {spendLimit === null
                          ? "none"
                          : formatMicroUsd(spendLimit)}{" "}
                        per run
                      </span>
                    </div>
                  </li>
                </ul>
              )}
            </section>
            <section className="nex-activity-section" aria-label="Context pressure">
              <h3 className="nex-activity-section-title">Context</h3>
              {activeConversationId === null ? (
                <p className="nex-activity-muted">
                  Open a conversation to see its context pressure.
                </p>
              ) : contextError !== null ? (
                <p className="nex-activity-error" role="alert">
                  {contextError}
                </p>
              ) : context === null ? (
                <p className="nex-activity-muted">Context stats unavailable.</p>
              ) : (
                <div className="nex-activity-row">
                  <div className="nex-activity-row-main">
                    <span className="nex-activity-row-title">{context.title}</span>
                    <span className="nex-activity-pill">
                      {context.has_token_data
                        ? `${context.usage_percent}% used`
                        : "usage n/a"}
                    </span>
                  </div>
                  <div className="nex-activity-row-meta">
                    <span>
                      {context.message_count} messages ·{" "}
                      {context.tool_call_count} tool calls
                    </span>
                    <span>{formatMicroUsd(context.total_cost_micro_usd)}</span>
                  </div>
                  {context.has_token_data && (
                    <div
                      className="nex-activity-meter"
                      role="progressbar"
                      aria-valuenow={context.usage_percent}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-label="Context window used"
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
            <section className="nex-activity-section" aria-label="Git status">
              <h3 className="nex-activity-section-title">Git</h3>
              {gitError !== null ? (
                <p className="nex-activity-muted">{gitError}</p>
              ) : git === null ? (
                <p className="nex-activity-muted">Git status unavailable.</p>
              ) : (
                <div className="nex-activity-row">
                  <div className="nex-activity-row-main">
                    <span className="nex-activity-row-title">
                      {git.branch ?? "Detached HEAD"}
                    </span>
                    <span className="nex-activity-pill">
                      {git.files.length === 0 ? "Clean" : "Dirty"}
                    </span>
                  </div>
                  <div className="nex-activity-row-meta">
                    <span>
                      {git.files.length === 0
                        ? "No changed files"
                        : `${git.files.length} changed file${git.files.length === 1 ? "" : "s"}${
                            git.files_overflow > 0 ? ` (+${git.files_overflow} more)` : ""
                          }`}
                    </span>
                    <span>
                      {git.commits.length === 0
                        ? "No commits"
                        : `${git.commits.length} recent commit${git.commits.length === 1 ? "" : "s"}`}
                    </span>
                  </div>
                </div>
              )}
            </section>
            <section className="nex-activity-section" aria-label="Feature flags">
              <h3 className="nex-activity-section-title">Flags</h3>
              {flagsError !== null ? (
                <p className="nex-activity-error" role="alert">
                  {flagsError}
                </p>
              ) : flagEntries.length === 0 ? (
                <p className="nex-activity-muted">No flags reported.</p>
              ) : (
                <ul className="nex-activity-list">
                  {flagEntries.map(([name, flag]) => (
                    <li key={name} className="nex-activity-row">
                      <div className="nex-activity-row-main">
                        <span className="nex-activity-row-title">{name}</span>
                        <span className="nex-activity-pill">
                          {flag.enabled ? "On" : "Off"}
                        </span>
                      </div>
                      <div className="nex-activity-row-meta">
                        <span>source: {flag.source}</span>
                        <span>{flag.enforced ? "enforced" : "not enforced"}</span>
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
