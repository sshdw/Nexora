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

import { useEffect, useState } from "react";

import { useComposerPopoverLayout } from "../lib/composerPopover";
import {
  conversationContextStats,
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
}: ContextRingProps) {
  const { t } = useStrings();
  const [stats, setStats] = useState<ConversationContextStats | null>(null);
  const layout = useComposerPopoverLayout();

  useEffect(() => {
    let cancelled = false;
    conversationContextStats(conversationId)
      .then((next) => {
        if (!cancelled) setStats(next);
      })
      .catch(() => {
        if (!cancelled) setStats(null);
      });
    return () => {
      cancelled = true;
    };
  }, [conversationId, revision]);

  const known = stats !== null && stats.has_token_data;
  const percent = known && stats !== null ? stats.usage_percent : null;
  const usage =
    known && stats !== null
      ? `${formatCompactTokens(stats.total_tokens)} / ${formatCompactTokens(stats.context_limit)} (${Math.round(stats.usage_percent)}%)`
      : t("common.na");
  const label =
    known && stats !== null
      ? t("conv.contextAria", { percent: Math.round(stats.usage_percent) })
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
          data-context={known ? "value" : "na"}
        >
          <ContextRingIcon percent={percent} size={16} />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        side="top"
        sideOffset={layout.sideOffset}
        collisionPadding={8}
        className="nex-composer-context-menu"
        style={
          layout.maxHeight === undefined
            ? undefined
            : { maxHeight: layout.maxHeight }
        }
      >
        <PopoverHeader>
          <PopoverTitle>{t("conv.contextWindow")}</PopoverTitle>
        </PopoverHeader>
        <p className="nex-composer-context-usage">{usage}</p>
        {/* The body the removed Conversation/Context switch carried: the
            same stats object, so there is no second fetch. It mounts with
            the popover, which is what keeps the runs/steps/diffs read
            closed until someone asks for it. */}
        <ContextPanel conversationId={conversationId} stats={stats} />
      </PopoverContent>
    </Popover>
  );
}
