//! Last-session memory — the shell reopens what you were reading (O-13).
//!
//! Tabs are session state, not rows: `useConversationTabs` deliberately does
//! not persist its list (a stale id would have to be validated against a list
//! that has not loaded yet). What survives a restart is the ONE conversation
//! the user was last in — the register's O-13 option (b), "restore last
//! session only" — which is enough to stop the app from opening on a screen
//! that claims there is no history while the database holds plenty.
//!
//! localStorage, like the other device preferences (`nexora.locale`,
//! `nexora.onboarding.completed`): per-device, never blocks startup, and a
//! rejected read or write degrades to "nothing remembered" instead of an
//! error. Validation stays with the caller — this module stores an id, and
//! only App knows which ids still exist.

const KEY = "nexora.lastSession";

export function readLastSession(): number | null {
  try {
    const raw = window.localStorage.getItem(KEY);
    if (raw === null) return null;
    const value = Number(raw);
    return Number.isInteger(value) && value > 0 ? value : null;
  } catch {
    return null;
  }
}

export function rememberSession(id: number | null): void {
  try {
    if (id === null) window.localStorage.removeItem(KEY);
    else window.localStorage.setItem(KEY, String(id));
  } catch {
    // Best-effort: a rejected write only means the next launch forgets.
  }
}
