//! Keyboard-shortcuts help dialog (hotkeys): grouped table over the
//! SHORTCUTS registry (src/lib/shortcuts.ts) — the dialog displays, the
//! registry defines, so the two cannot drift apart.
//!
//! M3E mapping (skill precedence: contract + PO full-M3E, then OFFICIAL):
//! dialog enter = scale+fade on the OFFICIAL medium2 · emphasized-decelerate
//! anchor (same grammar as the palette — CommandPalette.tsx:1-11, via
//! .nex-pop-enter in components.css), rows are calm reading chrome (tone
//! steps + spacing group items, no per-row borders — contract §Density),
//! reduced motion = final frame instantly via the global motion.css gate.
//! All visuals are --nex-sys-* / --motion-* tokens (src/styles/shortcuts.css)
//! — zero raw values.
//!
//! A11y: ModalShell owns Esc + focus trap/return (Modal.tsx precedent —
//! shortcut:dialog.close, shortcut:dialog.trap). Content is a grouped
//! reference table (group headings + rows); kbd chips are presentational
//! (aria-hidden) with the key names also in the row's accessible name.

import { SHORTCUT_GROUPS, shortcutsInGroup } from "../lib/shortcuts";
import ModalShell from "./Modal";

export interface ShortcutsDialogProps {
  onClose: () => void;
}

export default function ShortcutsDialog({ onClose }: ShortcutsDialogProps) {
  return (
    <ModalShell title="Keyboard shortcuts" onClose={onClose}>
      <div className="nex-shortcuts nex-pop-enter">
        <ul className="nex-shortcuts-groups">
          {SHORTCUT_GROUPS.map((group) => (
            <li key={group} className="nex-shortcuts-group">
              <h4 className="nex-shortcuts-heading">{group}</h4>
              <ul className="nex-shortcuts-rows">
                {shortcutsInGroup(group).map((entry) => (
                  <li
                    key={entry.id}
                    className="nex-shortcuts-row"
                    aria-label={`${entry.keys.join(", ")}: ${entry.description} (${entry.scope})`}
                  >
                    <span className="nex-shortcuts-keys" aria-hidden="true">
                      {entry.keys.map((label) => (
                        <kbd key={label}>{label}</kbd>
                      ))}
                    </span>
                    <span className="nex-shortcuts-text">
                      <span className="nex-shortcuts-desc">{entry.description}</span>
                      <span className="nex-shortcuts-scope">{entry.scope}</span>
                    </span>
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
        <p className="nex-shortcuts-hints" aria-hidden="true">
          <span>
            <kbd>esc</kbd> close
          </span>
        </p>
      </div>
    </ModalShell>
  );
}
