//! Collapsible provider-native thinking/reasoning block (Stage 5 PR 5B).
//!
//! Renders ONLY real provider-native thinking/reasoning emitted during streaming
//! or retained on the turn/run. Never fabricates or synthesizes placeholder
//! reasoning text: when unavailable, either renders an explicit honest absence
//! line (`showEmpty`) or renders nothing.

import { useId, useState } from "react";

import { useStrings } from "../lib/useLocale";
import { ChevronDownIcon } from "./icons";

export interface ThinkingBlockProps {
  /** Real provider-native thinking/reasoning text, or null/undefined if none. */
  thinking?: string | null;
  /** True while the turn or run is actively streaming thinking tokens. */
  streaming?: boolean;
  /** Provider capability flag (`false` -> explicit unsupported copy). */
  supportsThinking?: boolean | null;
  /** When true and no thinking text is available, render a muted honest
   *  absence line instead of omitting the block. */
  showEmpty?: boolean;
  /** Initial expanded state when `streaming` is false. */
  defaultOpen?: boolean;
}

export default function ThinkingBlock({
  thinking,
  streaming = false,
  supportsThinking = null,
  showEmpty = false,
  defaultOpen = false,
}: ThinkingBlockProps) {
  const { t } = useStrings();
  const bodyId = useId();
  const [userExpanded, setUserExpanded] = useState<boolean | null>(null);

  const hasContent = Boolean(thinking && thinking.trim().length > 0);
  const open =
    userExpanded !== null ? userExpanded : Boolean(streaming || defaultOpen);

  if (!hasContent) {
    if (!showEmpty) return null;
    return (
      <p className="nex-thinking-empty" data-thinking-state="absent">
        {supportsThinking === false
          ? t("conv.thinkingUnsupported")
          : t("conv.thinkingUnavailable")}
      </p>
    );
  }

  return (
    <div
      className={
        "nex-thinking-block" +
        (streaming ? " is-streaming" : "") +
        (open ? " is-open" : "")
      }
      data-thinking-state={streaming ? "streaming" : "present"}
    >
      <button
        type="button"
        className="nex-thinking-toggle"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => setUserExpanded(!open)}
      >
        <span className="nex-thinking-label">
          {streaming ? t("conv.thinkingStreaming") : t("conv.thinkingTitle")}
        </span>
        <ChevronDownIcon className="nex-thinking-chevron" />
      </button>
      <pre id={bodyId} className="nex-thinking-body" hidden={!open}>
        {thinking}
      </pre>
    </div>
  );
}
