//! Multi-conversation tab + split state for the M3E shell.
//!
//! Extension of the single-`selectedId` mechanism in `src/App.tsx`: where the
//! shell tracked one open conversation id, this store tracks an ordered list
//! of open ids (`openIds`) with one primary (`activeId`) and an optional
//! second conversation shown side by side (`splitId`). Invariant: both
//! `activeId` and `splitId` are always members of `openIds` (or null when no
//! tab is open). Closing the active tab activates the nearest surviving
//! neighbour (previous position, else next), so focus never lands on nothing
//! while tabs remain.
//!
//! Keyboard map (chosen to avoid every pre-existing shortcut — the only
//! existing keys are composer Enter/Shift+Enter in
//! `ConversationView.tsx:463`, rename Enter/Escape in
//! `ConversationItem.tsx:79-85`, segmented arrows/Home/End in
//! `M3SegmentedGroup.tsx:79-103`, toolbar/modal Escape in
//! `M3Toolbar.tsx:71-74` and `Modal.tsx:113-114`; none use modifiers, so all
//! modified combos below are collision-free):
//!   Ctrl+Tab / Ctrl+PageDown .... next tab
//!   Ctrl+Shift+Tab / Ctrl+PageUp  previous tab
//!   Alt+1..Alt+8 ................ jump to tab by position (Alt+9 = last)
//!   Alt+W ....................... close the active tab
//!   Alt+S ....................... toggle split (opens the most-recent other
//!                                 tab when closed, closes the pane when open)
//!   Alt+Z ....................... toggle zen reading mode (Esc exits; Esc is
//!                                 already the dismiss key for modals,
//!                                 toolbars and rename inputs, so zen yields
//!                                 to any open dialog — see App.tsx)
//!
//! Deferred (documented, not implemented): drag-and-drop tab reorder and
//! persistence across restart. Persistence was evaluated against the existing
//! settings store (`getSetting`/`setSetting` in `src/lib/tauri.ts`) and
//! deferred deliberately: tab ids are backend row ids that can be
//! archived/deleted between launches, so a restored id list would need a
//! validation pass and a stale-id UX the shell does not have yet. Revisit
//! once the shell owns an invalid-selection pattern.

import { useCallback, useState } from "react";

export interface ConversationTabsStore {
  /** Open tabs in open order (oldest first). */
  openIds: number[];
  /** Primary pane conversation, or null when no tab is open. */
  activeId: number | null;
  /** Secondary split-pane conversation, or null when split is closed. */
  splitId: number | null;
  /** Open a conversation as a tab (no-op reorder) and make it active. */
  open: (id: number) => void;
  /** Activate an already-open tab. */
  activate: (id: number) => void;
  /** Close a tab; activates the nearest surviving neighbour. */
  close: (id: number) => void;
  /** Show a conversation in the secondary pane (opened first if needed). */
  openInSplit: (id: number) => void;
  /** Close the secondary pane (the tab itself stays open). */
  closeSplit: () => void;
  /** Advance the primary pane to the next tab (wraps). */
  next: () => void;
  /** Move the primary pane to the previous tab (wraps). */
  prev: () => void;
  /** Jump the primary pane to the tab at `index` (clamped). */
  jumpTo: (index: number) => void;
  /** Drop ids that no longer exist (deleted conversations). */
  prune: (validIds: ReadonlySet<number>) => void;
}

export function useConversationTabs(): ConversationTabsStore {
  const [openIds, setOpenIds] = useState<number[]>([]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [splitId, setSplitId] = useState<number | null>(null);

  const open = useCallback((id: number) => {
    setOpenIds((prev) => (prev.includes(id) ? prev : [...prev, id]));
    setActiveId(id);
  }, []);

  const activate = useCallback((id: number) => {
    // Activation of an unknown id would break the membership invariant,
    // so unknown ids are opened (same as clicking a sidebar row).
    setOpenIds((prev) => (prev.includes(id) ? prev : [...prev, id]));
    setActiveId(id);
  }, []);

  const close = useCallback(
    (id: number) => {
      const index = openIds.indexOf(id);
      if (index === -1) return;
      const next = openIds.filter((openId) => openId !== id);
      setOpenIds(next);
      if (activeId === id) {
        // Neighbour activation: prefer the tab that slides into the closed
        // tab's position, else the new last tab; empty list clears active.
        setActiveId(
          next.length === 0 ? null : (next[Math.min(index, next.length - 1)] as number),
        );
      }
      // A closed conversation cannot stay in the split pane.
      if (splitId === id) setSplitId(null);
    },
    [openIds, activeId, splitId],
  );

  const openInSplit = useCallback(
    (id: number) => {
      if (!openIds.includes(id)) setOpenIds([...openIds, id]);
      // The secondary pane must differ from the primary: splitting the
      // active conversation onto itself is a no-op (avoids two panes
      // fighting over one draft — see App.tsx per-pane draft map).
      if (activeId === null) {
        setActiveId(id);
      } else if (activeId !== id) {
        setSplitId(id);
      }
    },
    [openIds, activeId],
  );

  const closeSplit = useCallback(() => {
    setSplitId(null);
  }, []);

  const step = useCallback(
    (direction: 1 | -1) => {
      if (openIds.length === 0) return;
      const anchor =
        activeId !== null && openIds.includes(activeId) ? activeId : (openIds[0] as number);
      const nextIndex =
        (openIds.indexOf(anchor) + direction + openIds.length) % openIds.length;
      setActiveId(openIds[nextIndex] as number);
    },
    [openIds, activeId],
  );

  const next = useCallback(() => step(1), [step]);
  const prev = useCallback(() => step(-1), [step]);

  const jumpTo = useCallback(
    (index: number) => {
      if (openIds.length === 0) return;
      const clamped = Math.max(0, Math.min(index, openIds.length - 1));
      setActiveId(openIds[clamped] as number);
    },
    [openIds],
  );

  const prune = useCallback(
    (validIds: ReadonlySet<number>) => {
      if (openIds.every((id) => validIds.has(id))) return;
      const next = openIds.filter((id) => validIds.has(id));
      setOpenIds(next);
      if (activeId !== null && !validIds.has(activeId)) {
        setActiveId(next.length > 0 ? (next[next.length - 1] as number) : null);
      }
      if (splitId !== null && !validIds.has(splitId)) setSplitId(null);
    },
    [openIds, activeId, splitId],
  );

  return {
    openIds,
    activeId,
    splitId,
    open,
    activate,
    close,
    openInSplit,
    closeSplit,
    next,
    prev,
    jumpTo,
    prune,
  };
}
