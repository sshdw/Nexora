//! Permanent compact session strip. Stage5 PR5A supersedes D-04's
//! two-tabs-only strip: identity, switching and close stay here at every
//! session count, with exactly one trailing Split control. Overflow and
//! automatic keyboard activation stay scoped to the tablist. Workspace
//! selection belongs to Composer; export belongs to Sessions and palette.

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
  splitOpen: boolean;
  onToggleSplit: () => void;
}

export default function ConversationTabs({
  tabs,
  activeId,
  splitId,
  onActivate,
  onClose,
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
    index: number
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
        role={tabs.length > 0 ? "tablist" : undefined}
        aria-label={tabs.length > 0 ? t("tabs.list") : undefined}
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
                "nex-tab nex-tab-enter" + (selected ? " is-active" : "")
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
          disabled={
            !splitOpen &&
            (activeId === null || !tabs.some((tab) => tab.id !== activeId))
          }
          title={t("tabs.splitTitle")}
        >
          {splitOpen ? t("app.splitClose") : t("app.split")}
        </M3Button>
      </div>
    </div>
  );
}
