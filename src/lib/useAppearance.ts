//! Appearance preferences hook (Phase 10.8 — FR-012, SRS §11).
//!
//! Persists the theme under the `appearance.theme` app-settings key through
//! the existing settings commands, so the choice survives application
//! restarts. Only the two defined themes (`dark`, `light`) are accepted:
//! any other stored or requested value is rejected and the default (dark)
//! applies — invalid values never reach persistence.

import { useCallback, useEffect, useState } from "react";

import { getSetting, setSetting } from "./tauri";

export type Theme = "dark" | "light";

/** Setting key backing the persisted appearance preference (FR-012). */
const THEME_KEY = "appearance.theme";

const THEMES: readonly Theme[] = ["dark", "light"];

function isTheme(value: string | null): value is Theme {
  return value !== null && (THEMES as readonly string[]).includes(value);
}

/** Apply (or clear, for the dark default) the root `data-theme` attribute. */
function applyTheme(theme: Theme): void {
  if (theme === "light") {
    document.documentElement.dataset.theme = "light";
  } else {
    delete document.documentElement.dataset.theme;
  }
}

export interface AppearanceStore {
  /** The active theme; `dark` until a valid persisted value loads. */
  theme: Theme;
  /** Validate, persist, and apply a theme selection. */
  setTheme: (theme: Theme) => Promise<void>;
}

export function useAppearance(): AppearanceStore {
  const [theme, setThemeState] = useState<Theme>("dark");

  // Load the persisted theme once at startup so the visual preference is
  // restored before (or as) the first paint settles. A missing value, an
  // invalid value, or a read failure all resolve to the dark default.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const stored = await getSetting(THEME_KEY);
        if (!cancelled && isTheme(stored)) {
          setThemeState(stored);
          applyTheme(stored);
        }
      } catch {
        // Offline-safe: settings live in local SQLite; a transient read
        // failure leaves the dark default without blocking startup.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const setTheme = useCallback(async (next: Theme): Promise<void> => {
    // Reject undefined themes instead of inventing behavior for them.
    if (!isTheme(next)) return;
    await setSetting(THEME_KEY, next);
    setThemeState(next);
    applyTheme(next);
  }, []);

  return { theme, setTheme };
}
