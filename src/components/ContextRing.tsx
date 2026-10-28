//! Context-window ring + popover (D4).
//!
//! A small donut in the composer's bottom row, left of the model trigger.
//! The source is `conversationContextStats`; the ring refreshes when the
//! conversation changes and whenever the pane's run revision moves (run
//! events), never on a timer.
//!
//! `has_token_data: false` is a real state, not zero: the ring shows a
//! dashed track and both the button name and the numbers read `n/a`.
//! Nothing here estimates tokens, cache hit rate, balance or quota — the
//! product does not have those numbers.

import { useEffect, useState } from "react";

import {
  conversationContextStats,
  type ConversationContextStats,
} from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
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

export default function ContextRing({ conversationId, revision }: ContextRingProps) {
  const { t } = useStrings();
  const [stats, setStats] = useState<ConversationContextStats | null>(null);

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
  const usage = known && stats !== null
    ? `${formatCompactTokens(stats.total_tokens)} / ${formatCompactTokens(stats.context_limit)} (${Math.round(stats.usage_percent)}%)`
    : t("common.na");
  const label = known && stats !== null
    ? t("conv.contextAria", { percent: Math.round(stats.usage_percent) })
    : t("conv.contextAriaNa");

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
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
        sideOffset={8}
        className="nex-composer-context-menu"
      >
        <PopoverHeader>
          <PopoverTitle>{t("conv.contextWindow")}</PopoverTitle>
        </PopoverHeader>
        <p className="nex-composer-context-usage">{usage}</p>
        <dl className="nex-composer-context-facts">
          <div className="nex-composer-context-fact">
            <dt>{t("conv.contextProvider")}</dt>
            <dd>{stats?.provider_display ?? stats?.provider ?? t("common.na")}</dd>
          </div>
          <div className="nex-composer-context-fact">
            <dt>{t("conv.contextModel")}</dt>
            <dd>{stats?.model ?? t("common.na")}</dd>
          </div>
        </dl>
      </PopoverContent>
    </Popover>
  );
}
