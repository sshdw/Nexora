//! The Activity tab body: the filter + the merged run/conversation feed
//! (AC-208 split, per the review §3.1 plan — moved verbatim from
//! `ActivityHealthPanel.tsx`, whose `rows`/`titles` memos render here).

import { useMemo } from "react";

import type { ActivityFeed, Conversation } from "../../lib/tauri";
import { formatRelativeTime } from "../../lib/format";
import { useStrings } from "../../lib/useLocale";
import M3LoadingIndicator from "../M3LoadingIndicator";
import M3SegmentedGroup from "../M3SegmentedGroup";
import { formatMicroUsd, runStatusLabel } from "./format";
import type { ActivityKind } from "./tabs";

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

export interface ActivityTabProps {
  feed: ActivityFeed | null;
  /** Wider (server-max) page backing the "spend" filter only; `null` =
   * unavailable → the filter falls back to the main `feed` page. */
  spendFeed: ActivityFeed | null;
  conversations: Conversation[];
  loading: boolean;
  error: string | null;
  /** Filter state is owned by the parent: the tab unmounts on every tab
   * switch, and the chosen filter must survive the round trip exactly as
   * it did when the state lived in the parent. */
  kind: ActivityKind;
  onKindChange: (kind: ActivityKind) => void;
}

export default function ActivityTab({
  feed,
  spendFeed,
  conversations,
  loading,
  error,
  kind,
  onKindChange,
}: ActivityTabProps) {
  const { locale, t, tp: tpn } = useStrings();

  const titles = useMemo(() => {
    const map = new Map<number, string>();
    for (const conv of conversations) map.set(conv.id, conv.title);
    return map;
  }, [conversations]);

  const rows = useMemo<FeedRow[]>(() => {
    const toRunRow = (
      run: NonNullable<ActivityFeed["runs"]>[number]
    ): FeedRow => {
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
          convTitle !== undefined
            ? t("activity.runIn", { title: convTitle })
            : t("activity.runHash", { id: run.run_id }),
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
        detail:
          conv.status === "archived"
            ? t("common.archived")
            : t("common.active"),
        time: conv.created_at,
      });
    }
    merged.sort((a, b) => b.at - a.at);
    if (kind === "runs") return merged.filter((row) => row.kind === "run");
    if (kind === "conversations")
      return merged.filter((row) => row.kind === "conversation");
    return merged;
  }, [feed, spendFeed, conversations, titles, kind, locale, t, tpn]);

  return (
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
          onChange={onKindChange}
        />
      </div>
      {loading ? (
        <M3LoadingIndicator label={t("activity.loadingActivity")} />
      ) : error !== null ? (
        <p className="nex-activity-error" role="alert">
          {error}
        </p>
      ) : rows.length === 0 ? (
        <div className="nex-activity-empty">
          <p className="nex-activity-empty-title">{t("activity.emptyTitle")}</p>
          <p className="nex-activity-empty-text">{t("activity.emptyText")}</p>
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
  );
}
