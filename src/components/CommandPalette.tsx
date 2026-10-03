//! Ctrl+K command palette (NL-palette): keyboard-first fuzzy launcher over
//! the command registry (src/lib/commands.ts).
//!
//! M3E mapping (skill precedence: contract + PO full-M3E, then OFFICIAL):
//! dialog enter = scale+fade on the OFFICIAL medium2 · emphasized-decelerate
//! anchor (skill motion.md §7 "menu opens with scale+fade from anchor
//! (medium2)"), list navigation = instant highlight swap (calm persistent
//! chrome — same grammar as the tab strip's instant tone-step selection,
//! ConversationTabs.tsx:14-17), reduced motion = final frame instantly via
//! the global motion.css gate. All visuals are --nex-sys-* / --motion-*
//! tokens (src/styles/palette.css) — zero raw values.
//!
//! A11y: combobox input owns the query; results are a grouped listbox with
//! aria-activedescendant; ArrowUp/Down/Home/End move, Enter runs the
//! highlighted command (the registry wraps the existing UI handler, so
//! Enter === clicking), Escape closes with focus return (ModalShell owns
//! Esc + restoration). The palette input autofocuses on open; the invoker
//! regains focus on close.

import { useEffect, useMemo, useRef, useState } from "react";

import {
  filterCommands,
  getMru,
  type PaletteCommand,
} from "../lib/commands";
import ModalShell from "./Modal";

export interface CommandPaletteProps {
  commands: PaletteCommand[];
  onClose: () => void;
  /** Run a chosen command. The parent closes the palette first and runs
   * the command after unmount, so focus moves (composer, dialogs) land
   * on live, non-inert targets. */
  onRun: (command: PaletteCommand) => void;
}

export default function CommandPalette({ commands, onClose, onRun }: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [highlight, setHighlight] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const onRunRef = useRef(onRun);
  onRunRef.current = onRun;

  const mru = useMemo(() => getMru(), []);
  const results = useMemo(
    () => filterCommands(query, commands, mru),
    [query, commands, mru],
  );

  // Clamp the highlight as the result set shrinks/grows.
  useEffect(() => {
    setHighlight((prev) => {
      if (results.length === 0) return 0;
      return Math.min(prev, results.length - 1);
    });
  }, [results.length]);

  // Autofocus the query input on open (typing starts immediately).
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Keep the highlighted row visible (instant nearest-edge scroll — calm
  // chrome, no smooth panning).
  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>(`[data-index="${highlight}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [highlight]);

  const runHighlighted = () => {
    const hit = results[highlight];
    if (!hit) return;
    onRunRef.current(hit.command);
  };

  // shortcut:palette.navigate / shortcut:palette.edges / shortcut:palette.run
  // — scoped keys stay inline (results focus IS the scope). Esc is owned by
  // ModalShell (shortcut:dialog.close).
  const handleKeyDown = (event: React.KeyboardEvent) => {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        setHighlight((prev) =>
          results.length === 0 ? 0 : (prev + 1) % results.length,
        );
        break;
      case "ArrowUp":
        event.preventDefault();
        setHighlight((prev) =>
          results.length === 0
            ? 0
            : (prev - 1 + results.length) % results.length,
        );
        break;
      case "Home":
        event.preventDefault();
        setHighlight(0);
        break;
      case "End":
        event.preventDefault();
        setHighlight(Math.max(0, results.length - 1));
        break;
      case "Enter":
        event.preventDefault();
        runHighlighted();
        break;
    }
  };

  const activeId =
    results.length === 0 ? undefined : `nex-palette-option-${highlight}`;
  // Group consecutive same-section hits so section labels read as group
  // headers (listbox > group > option — AT users hear the grouping).
  const groups: Array<{
    section: string;
    headerId: string;
    indices: number[];
  }> = [];
  results.forEach((hit, index) => {
    const tail = groups[groups.length - 1];
    if (tail && tail.section === hit.command.section) {
      tail.indices.push(index);
    } else {
      groups.push({
        section: hit.command.section,
        headerId: `nex-palette-section-${groups.length}`,
        indices: [index],
      });
    }
  });

  return (
    <ModalShell title="Command palette" onClose={onClose} align="top">
      <div className="nex-palette nex-pop-enter">
        <input
          ref={inputRef}
          type="text"
          role="combobox"
          aria-expanded="true"
          aria-controls="nex-palette-listbox"
          aria-activedescendant={activeId}
          aria-label="Type a command"
          className="nex-input nex-palette-input"
          placeholder="Type a command…"
          value={query}
          autoComplete="off"
          spellCheck={false}
          onChange={(event) => {
            setQuery(event.target.value);
            setHighlight(0);
          }}
          onKeyDown={handleKeyDown}
        />
        {results.length === 0 ? (
          <p className="nex-palette-empty" role="status">
            No matching commands.
          </p>
        ) : (
          <ul
            ref={listRef}
            id="nex-palette-listbox"
            role="listbox"
            aria-label="Matching commands"
            className="nex-palette-list"
          >
            {groups.map((group) => (
              <li
                key={group.headerId}
                role="group"
                aria-labelledby={group.headerId}
              >
                <span id={group.headerId} className="nex-palette-section">
                  {group.section}
                </span>
                {group.indices.map((index) => (
                  <div
                    key={results[index]?.command.id ?? index}
                    id={`nex-palette-option-${index}`}
                    data-index={index}
                    role="option"
                    aria-selected={index === highlight}
                    className={
                      "nex-palette-item" +
                      (index === highlight ? " is-active" : "")
                    }
                    onMouseEnter={() => setHighlight(index)}
                    onClick={() => {
                      const hit = results[index];
                      if (hit) onRunRef.current(hit.command);
                    }}
                  >
                    <span className="nex-palette-item-title">
                      {results[index]?.command.title}
                    </span>
                  </div>
                ))}
              </li>
            ))}
          </ul>
        )}
        <p className="nex-palette-hints" aria-hidden="true">
          <span>
            <kbd>↑</kbd>
            <kbd>↓</kbd> navigate
          </span>
          <span>
            <kbd>↵</kbd> run
          </span>
          <span>
            <kbd>esc</kbd> close
          </span>
        </p>
      </div>
    </ModalShell>
  );
}
