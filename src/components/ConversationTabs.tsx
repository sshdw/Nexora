//! Conversation tab strip: docked M3E tab row for the main column.
//!
//! Placement (shell-density decision): the strip docks as the first row of
//! `.nex-main`, directly above the per-pane conversation header — persistent
//! calm chrome like the slim header below it (contract §Shell: docked rows
//! for persistent actions). It never overlaps the sidebar (which keeps its
//! own conversation list as the opener) or the composer.
//!
//! Grammar follows the `M3SegmentedGroup` tabs precedent
//! (`M3SegmentedGroup.tsx:12-15`): `tablist`/`tab` roles, automatic
//! activation on Left/Right/Home/End, `aria-selected` for selection. Each tab
//! carries its own close button (Alt+W closes the active tab — see
//! `useConversationTabs.ts` for the full shortcut map). Overflow scrolls
//! horizontally inside the strip; the active tab scrolls into view on
//! selection. New tabs enter with the expressive fast-spatial spring
//! (create moment); switching swaps content instantly with a tone-step
//! selection change — calm persistent chrome, no choreography on select.
//! Reduced motion collapses the enter animation via the motion.css gate.

import { useEffect, useRef } from "react";

import M3Button from "./M3Button";
import M3IconButton from "./M3IconButton";
import { useStrings } from "../lib/useLocale";
import { CloseIcon } from "./icons";

export interface TabEntry {
  id: number;
  title: string;
  archived: boolean;
}

export interface ConversationTabsProps {
  tabs: TabEntry[];
  activeId: number | null;
  splitId: number | null;
  onActivate: (id: number) => void;
  onClose: (id: number) => void;
  onNewConversation: () => void;
  creating: boolean;
  zen: boolean;
  onToggleZen: () => void;
  splitOpen: boolean;
  onToggleSplit: () => void;
}

export default function ConversationTabs({
  tabs,
  activeId,
  splitId,
  onActivate,
  onClose,
  onNewConversation,
  creating,
  zen,
  onToggleZen,
  splitOpen,
  onToggleSplit,
}: ConversationTabsProps) {
  const { t } = useStrings();
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);

  // Keep the active tab visible inside the overflowing strip (instant,
  // nearest-edge scroll — no smooth panning on calm chrome). Depends on a
  // joined-id signature, not the `tabs` array: App rebuilds that array every
  // render, and array identity would re-scroll on every render.
  const tabIdentity = tabs.map((tab) => tab.id).join(",");
  useEffect(() => {
    const ids = tabIdentity.length === 0 ? [] : tabIdentity.split(",");
    const index = activeId === null ? -1 : ids.indexOf(String(activeId));
    if (index !== -1) {
      tabRefs.current[index]?.scrollIntoView({
        block: "nearest",
        inline: "nearest",
      });
    }
  }, [tabIdentity, activeId]);

  if (tabs.length === 0) return null;

  const focusTab = (index: number) => {
    const count = tabs.length;
    const next = ((index % count) + count) % count;
    tabRefs.current[next]?.focus();
    const tab = tabs[next];
    if (tab && tab.id !== activeId) onActivate(tab.id);
  };

  // shortcut:tabstrip.move / shortcut:tabstrip.edges — scoped keys stay
  // inline (the focused tab IS the scope; see lib/shortcuts.ts).
  const handleTabKeyDown = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        event.preventDefault();
        focusTab(index + 1);
        break;
      case "ArrowLeft":
      case "ArrowUp":
        event.preventDefault();
        focusTab(index - 1);
        break;
      case "Home":
        event.preventDefault();
        focusTab(0);
        break;
      case "End":
        event.preventDefault();
        focusTab(tabs.length - 1);
        break;
    }
  };

  return (
    <div className="nex-tabstrip" role="region" aria-label={t("tabs.region")}>
      <div
        className="nex-tabstrip-list"
        role="tablist"
        aria-label={t("tabs.list")}
      >
        {tabs.map((tab, index) => {
          const selected = tab.id === activeId;
          const inSplit = tab.id === splitId;
          return (
            // Nav-style tabs: each tab is an independent button and panes are
            // plain title-labelled sections, not tabpanel-wired — so the
            // layout wrapper is presentational and the tablist owns the tab
            // buttons directly (owned-element relationship intact).
            <div
              key={tab.id}
              role="presentation"
              className={
                "nex-tab nex-tab-enter" +
                (selected ? " is-active" : "") +
                (inSplit ? " is-split" : "")
              }
            >
              <button
                ref={(element) => {
                  tabRefs.current[index] = element;
                }}
                type="button"
                role="tab"
                aria-selected={selected}
                aria-label={
                  tab.title +
                  (inSplit ? t("tabs.inSplitSuffix") : "") +
                  (tab.archived ? t("tabs.archivedSuffix") : "")
                }
                title={tab.title}
                tabIndex={selected ? undefined : -1}
                className="nex-tab-label"
                onClick={() => onActivate(tab.id)}
                onKeyDown={(event) => handleTabKeyDown(event, index)}
              >
                <span className="nex-tab-text">{tab.title}</span>
                {inSplit && (
                  <span className="nex-tab-split-mark" aria-hidden="true">
                    2
                  </span>
                )}
              </button>
              <M3IconButton
                size="sm"
                label={t("tabs.closeTab", { title: tab.title })}
                className="nex-tab-close"
                onClick={() => onClose(tab.id)}
              >
                <CloseIcon />
              </M3IconButton>
            </div>
          );
        })}
      </div>
      <div className="nex-tabstrip-actions">
        <M3Button
          variant="quiet"
          size="sm"
          onClick={onToggleSplit}
          aria-pressed={splitOpen}
          disabled={!splitOpen && tabs.length < 2}
          title={t("tabs.splitTitle")}
        >
          {splitOpen ? t("app.splitClose") : t("app.split")}
        </M3Button>
        <M3Button
          variant="quiet"
          size="sm"
          onClick={onToggleZen}
          aria-pressed={zen}
          title={t("tabs.zenTitle")}
        >
          {t("tabs.zen")}
        </M3Button>
        <M3Button
          variant="quiet"
          size="sm"
          onClick={onNewConversation}
          disabled={creating}
          title={t("tabs.newTitle")}
        >
          {t("tabs.new")}
        </M3Button>
      </div>
    </div>
  );
}
