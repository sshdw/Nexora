//! First-run onboarding state (simplified onboarding).
//!
//! Detection signal (documented choice): a fresh install has NO usable
//! provider (no `providers` row + keyring credential pair, i.e. nothing
//! `available` in `useProviders`) AND no workspace root yet. Both halves
//! are required: a keyless-but-workspaced (or keyed-but-workspaceless)
//! setup is an existing user mid-configuration, not a first run.
//!
//! Persistence decision (documented choice): `localStorage`, NOT the
//! backend settings store. A new backend settings key would need Rust-side
//! verification (allowlist/defaults) and this task is frontend-only by
//! mandate; `localStorage` is per-device like the other device prefs,
//! survives restarts, and never blocks startup (read failures degrade to
//! "not completed", writes are best-effort). Any dismissal — Finish, Skip,
//! Esc, backdrop — marks completion so the flow never re-shows; re-entry
//! is via the palette command + Settings entry, which open the flow
//! without clearing the flag.

import { useCallback, useState } from "react";

/** localStorage flag marking the onboarding flow completed/dismissed. */
const COMPLETED_KEY = "nexora.onboarding.completed";

export function hasCompletedOnboarding(): boolean {
  try {
    return window.localStorage.getItem(COMPLETED_KEY) === "1";
  } catch {
    return false;
  }
}

function persistOnboardingComplete(): void {
  try {
    window.localStorage.setItem(COMPLETED_KEY, "1");
  } catch {
    // Best-effort: a rejected write only means the flow may re-show once.
  }
}

export interface OnboardingStore {
  /** Whether the completion flag is set (flow must not auto-show). */
  completed: boolean;
  /** Whether the flow overlay is open. */
  open: boolean;
  /** Mark complete and close (Finish, Skip-all, Esc, backdrop). */
  dismiss: () => void;
  /** Open the flow (auto-show or explicit replay — never clears the flag). */
  replay: () => void;
}

export function useOnboarding(): OnboardingStore {
  const [completed, setCompleted] = useState<boolean>(() =>
    hasCompletedOnboarding(),
  );
  const [open, setOpen] = useState<boolean>(false);

  const dismiss = useCallback(() => {
    persistOnboardingComplete();
    setCompleted(true);
    setOpen(false);
  }, []);

  const replay = useCallback(() => {
    setOpen(true);
  }, []);

  return { completed, open, dismiss, replay };
}
