//! Appearance preferences hook (Phase 10.8 — FR-012, SRS §11).
//!
//! Persists the theme under the `appearance.theme` app-settings key through
//! the existing settings commands, so the choice survives application
//! restarts. Only the two defined themes (`dark`, `light`) are accepted:
//! any other stored or requested value is rejected and the OS-matched
//! default applies — invalid values never reach persistence.

import { useCallback, useEffect, useMemo, useState } from "react";

import { getSetting, setSetting } from "./tauri";

export type Theme = "dark" | "light";

/** Setting key backing the persisted appearance preference (FR-012). */
const THEME_KEY = "appearance.theme";

const THEMES: readonly Theme[] = ["dark", "light"];

function isTheme(value: string | null): value is Theme {
  return value !== null && (THEMES as readonly string[]).includes(value);
}

/** Apply the root `data-theme` attribute (explicit override hook).
 *
 * Both themes are set explicitly — never removed — so the persisted
 * choice wins over the dark first-paint default in
 * `src/styles/tokens.css`. `high-contrast` remains available as a
 * manual `data-theme="high-contrast"` / `.nex-theme-high-contrast`
 * override (token mechanism only; no toggle UI yet). */
function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
}

export interface AppearanceStore {
  /** The active theme; OS-matched default until a valid persisted value loads. */
  theme: Theme;
  /** Validate, persist, and apply a theme selection. */
  setTheme: (theme: Theme) => Promise<void>;
}

export function useAppearance(): AppearanceStore {
  const [theme, setThemeState] = useState<Theme>("dark");

  // Load the persisted theme once at startup so the visual preference is
  // restored before (or as) the first paint settles. A missing or
  // invalid value resolves to the OS-matched default (light when
  // prefers-color-scheme matches, else dark); a read failure keeps
  // the dark default. The
  // resolved theme is always written as an explicit attribute (single
  // light source lives in `src/styles/tokens.css`), so store state and
  // the painted theme can never disagree.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const stored = await getSetting(THEME_KEY);
        if (cancelled) return;
        if (isTheme(stored)) {
          setThemeState(stored);
          applyTheme(stored);
        } else if (
          typeof window !== "undefined" &&
          typeof window.matchMedia === "function"
        ) {
          const fallback = window.matchMedia("(prefers-color-scheme: light)")
            .matches
            ? "light"
            : "dark";
          setThemeState(fallback);
          applyTheme(fallback);
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

  // Memoized store identity (NEX-FE-002 audit): App passes the store
  // object as a prop, so a fresh literal every render re-rendered Settings.
  return useMemo(() => ({ theme, setTheme }), [theme, setTheme]);
}
