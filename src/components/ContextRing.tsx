//! Context-window ring + popover (D4 / R6-05 / R6-09).
//!
//! A small donut in the composer's bottom row, left of the model trigger.
//! The source is `conversationContextStats`; the ring refreshes when the
//! conversation changes and whenever the pane's run revision moves (run
//! events), never on a timer.
//!
//! `has_token_data: false` is a real state, not zero: the ring shows a
//! SOLID, faint boundary track and both the button name and the numbers
//! read `n/a`. The track is solid on purpose (R6-09) — a dashed ring reads
//! as a loading spinner — and stays visibly empty because there is no value
//! arc. Nothing here estimates tokens, cache hit rate, balance or quota —
//! the product does not have those numbers.
//!
//! Since R6-05 the popover is also the home of the context panel the stage
//! used to switch to: the stats this component already fetched are passed
//! straight into `ContextPanel`, and that body loads its heavy half (runs →
//! steps → diffs) only while the popover is open. No second stats fetch.

import { useCallback, useEffect, useState } from "react";

import { useComposerPopoverLayout } from "../lib/composerPopover";
import {
  compactConversationContext,
  conversationContextStats,
  type CompactionResult,
  type ConversationContextStats,
} from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import ContextPanel from "./ContextPanel";
import { Button } from "./ui/button";
import {
  Popover,
  PopoverContent,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
} from "./ui/popover";
import { ContextRingIcon } from "./icons";

export interface ContextRingProps {
  conversationId: number;
  /** Content signature from the pane: changes when a run event arrives. */
  revision: string;
  /** Optional active connection id for capability-backed window lookup. */
  selectedConnectionId?: string | null;
  /** Optional active model id for capability-backed window lookup. */
  selectedModel?: string | null;
  /** Notified after a manual compaction completes or schedules. */
  onCompacted?: (result: CompactionResult) => void;
}

/** Compact K/M formatting for the popover's numbers. */
export function formatCompactTokens(value: number): string {
  if (!Number.isFinite(value)) return "n/a";
  const abs = Math.abs(value);
  const trim = (n: number): string =>
    n >= 10 ? String(Math.round(n)) : n.toFixed(1).replace(/\.0$/, "");
  if (abs < 1_000) return String(Math.round(value));
  if (abs < 1_000_000) return `${trim(value / 1_000)}K`;
  return `${trim(value / 1_000_000)}M`;
}

export default function ContextRing({
  conversationId,
  revision,
  selectedConnectionId = null,
  selectedModel = null,
  onCompacted,
}: ContextRingProps) {
  const { t } = useStrings();
  const [stats, setStats] = useState<ConversationContextStats | null>(null);
  const [compacting, setCompacting] = useState(false);
  const [compactFeedback, setCompactFeedback] = useState<{
    kind: "status" | "error";
    text: string;
  } | null>(null);
  const layout = useComposerPopoverLayout();

  const fetchStats = useCallback(async () => {
    const hasSelection = Boolean(selectedConnectionId && selectedModel);
    return hasSelection
      ? conversationContextStats(conversationId, {
          connectionId: selectedConnectionId,
          modelId: selectedModel,
        })
      : conversationContextStats(conversationId);
  }, [conversationId, selectedConnectionId, selectedModel]);

  useEffect(() => {
    let cancelled = false;
    fetchStats()
      .then((next) => {
        if (!cancelled) setStats(next);
      })
      .catch(() => {
        if (!cancelled) setStats(null);
      });
    return () => {
      cancelled = true;
    };
  }, [fetchStats, revision]);

  useEffect(() => {
    setCompactFeedback(null);
  }, [conversationId]);

  const handleCompact = useCallback(async () => {
    if (compacting) return;
    setCompacting(true);
    setCompactFeedback(null);
    try {
      const hasSelection = Boolean(selectedConnectionId && selectedModel);
      const result = hasSelection
        ? await compactConversationContext(conversationId, {
            connectionId: selectedConnectionId,
            modelId: selectedModel,
          })
        : await compactConversationContext(conversationId);
      if (result.status === "scheduled") {
        setCompactFeedback({
          kind: "status",
          text: t("ctx.compactedScheduled"),
        });
      } else if (result.status === "nothing_to_compact") {
        setCompactFeedback({
          kind: "status",
          text: t("ctx.compactedNothing"),
        });
      } else {
        setCompactFeedback({
          kind: "status",
          text: t("ctx.compactedNotice", {
            summarized: result.summarized_messages,
            retained: result.retained_messages,
          }),
        });
      }
      onCompacted?.(result);
      const nextStats = await fetchStats().catch(() => null);
      if (nextStats) setStats(nextStats);
    } catch {
      setCompactFeedback({
        kind: "error",
        text: t("ctx.compactFailed"),
      });
    } finally {
      setCompacting(false);
    }
  }, [
    compacting,
    conversationId,
    fetchStats,
    onCompacted,
    selectedConnectionId,
    selectedModel,
    t,
  ]);

  const resolvedWindow =
    stats === null
      ? null
      : stats.context_window !== undefined
        ? stats.context_window
        : stats.context_limit > 0
          ? stats.context_limit
          : null;
  const windowUnknown = stats !== null && resolvedWindow === null;
  const known = stats !== null && stats.has_token_data && !windowUnknown;
  const percent = known && stats !== null ? stats.usage_percent : null;
  const usage =
    known && stats !== null && resolvedWindow !== null
      ? `${formatCompactTokens(stats.total_tokens)} / ${formatCompactTokens(resolvedWindow)} (${Math.round(stats.usage_percent)}%)`
      : windowUnknown && stats !== null && stats.has_token_data
        ? `${formatCompactTokens(stats.total_tokens)} / ${t("ctx.windowUnknown")}`
        : t("ctx.noTokenData");
  const usageEmpty = !known && !(windowUnknown && stats?.has_token_data);
  const label =
    known && stats !== null
      ? t("conv.contextAria", { percent: Math.round(stats.usage_percent) })
      : windowUnknown
        ? t("conv.contextAriaUnknown")
        : t("conv.contextAriaNa");

  return (
    <Popover
      onOpenChange={(next) => {
        // Measure before the body mounts: the offset is from the card's top
        // edge, and the height cap is the space above it (R6-05 defect fix).
        if (next) layout.measure();
      }}
    >
      <PopoverTrigger asChild>
        <Button
          ref={layout.triggerRef}
          variant="ghost"
          size="icon-sm"
          className="nex-composer-context-trigger"
          aria-label={label}
          title={t("conv.contextWindow")}
          data-context={known ? "value" : windowUnknown ? "unknown" : "na"}
        >
          <ContextRingIcon percent={percent} size={16} />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        side="top"
        sideOffset={layout.sideOffset}
        collisionPadding={8}
        className="nex-composer-context-menu p-2 gap-1.5"
        style={
          layout.maxHeight === undefined
            ? undefined
            : { maxHeight: layout.maxHeight }
        }
      >
        <PopoverHeader className="nex-context-popover-header flex-row items-center justify-between gap-2">
          <PopoverTitle>{t("conv.contextWindow")}</PopoverTitle>
          <Button
            type="button"
            variant="outline"
            size="xs"
            className="nex-context-compact-btn"
            aria-label={t("ctx.compactAria")}
            disabled={compacting || stats === null}
            onClick={() => void handleCompact()}
          >
            {compacting ? t("ctx.compacting") : t("ctx.compactAction")}
          </Button>
        </PopoverHeader>
        {known && stats !== null && resolvedWindow !== null ? (
          <div className="nex-composer-context-subheader">
            <span className="nex-composer-context-percent">
              {t("ctx.percentFull", {
                percent: Math.round(stats.usage_percent),
              })}
            </span>
            <span className="nex-composer-context-tokens">
              {t("ctx.tokensOfLimit", {
                used: formatCompactTokens(stats.total_tokens),
                limit: formatCompactTokens(resolvedWindow),
              })}
            </span>
          </div>
        ) : (
          <p
            className={
              "nex-composer-context-usage" + (usageEmpty ? " is-empty" : "")
            }
          >
            {usage}
          </p>
        )}
        {compactFeedback && (
          <p
            className={
              compactFeedback.kind === "error"
                ? "nex-composer-error"
                : "nex-context-compact-feedback"
            }
            role={compactFeedback.kind === "error" ? "alert" : "status"}
          >
            {compactFeedback.text}
          </p>
        )}
        {/* The body the removed Conversation/Context switch carried: the
            same stats object, so there is no second fetch. It mounts with
            the popover, which is what keeps the runs/steps/diffs read
            closed until someone asks for it. */}
        <ContextPanel conversationId={conversationId} stats={stats} />
      </PopoverContent>
    </Popover>
  );
}
