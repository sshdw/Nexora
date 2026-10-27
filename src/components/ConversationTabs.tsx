//! Conversation tab strip: docked M3E tab row for the main column.
//!
//! Placement (shell-density decision): the strip docks as the first row of
//! `.nex-main`, directly above the per-pane conversation header — persistent
//! calm chrome like the slim header below it (SYSTEM §4: docked rows for
//! persistent actions). It never overlaps the sidebar (which keeps its
//! own conversation list as the opener) or the composer.
//!
//! Grammar (MAP row 4, `tabs.md` `variant="line"`): quiet TEXT tabs —
//! `tablist`/`tab` roles, automatic activation on Left/Right/Home/End,
//! `aria-selected` for selection, a 2px tick under the active row plus a
//! weight/lightness step and nothing else. Each tab carries its own close
//! button (Alt+W closes the active tab — see `useConversationTabs.ts`
//! for the full shortcut map). Overflow scrolls horizontally inside
//! the strip; the active tab scrolls into view on selection. New tabs
//! enter with the expressive fast-spatial spring (create moment);
//! switching swaps content instantly with a tone-step selection change
//! — calm persistent chrome, no choreography on select. Reduced motion
//! collapses the enter animation via the motion.css gate.
//!
//! ONE Split entry point (SYSTEM §4, MAP row 5): it lives HERE and only
//! here. The pane header used to render a second one — a duplicate
//! control for one action, reachable from two places per pane, so a
//! split of two panes offered four identical buttons.
//!
//! TWO OR MORE SESSIONS ONLY (D-04 / C-02). The register settles the
//! duplication this row used to carry: at one open session the strip is
//! not rendered at all and the pane header owns the title; at two or more
//! the strip owns switching and the pane header drops its title (it keeps
//! the workspace chip and export). Rendering a single "tab" with its own
//! close × above an identical title was the same fact stated twice, 40 px
//! apart.
//!
//! The `+` is gone (D-04: "no `+` (New in Dock)"): creation is the Dock's
//! first destination and is reachable from every screen, so a second
//! create control inside the strip was a duplicate entry point for the one
//! action that already has the most prominent home.

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

  // One session is not a tab set (D-04): the pane header already names it.
  if (tabs.length < 2) return null;

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
                (selected ? " is-active" : "")
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
                {/* Split annotation: a quiet count badge, NOT a second
                    selection signal. It says which pane a tab is shown
                    in; the tick under the row says which is primary.
                    (`is-split` used to ride the wrapper with no rule
                    anywhere — the badge is the whole of it.) */}
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
      </div>
    </div>
  );
}
