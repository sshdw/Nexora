//! Locale state (EN+RU UI chrome): React context over the string catalog.
//!
//! Persistence decision (documented): `localStorage` (`nexora.locale`), NOT
//! the backend settings store — the onboarding completion flag
//! (`nexora.onboarding.completed`, useOnboarding.ts) sets the precedent, and
//! a new backend settings key would need Rust-side verification while this
//! task is frontend-only by mandate. `localStorage` is per-device like the
//! other device prefs, survives restarts, and never blocks startup (read
//! failures degrade to EN). Only `en`/`ru` are accepted; anything else
//! resolves to EN and never reaches persistence.

import { createContext, createElement, useCallback, useContext, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";

import {
  getLocale,
  isLocale,
  setCurrentLocale,
  tp,
  tr,
  type Locale,
  type PluralKey,
  type StringKey,
} from "./strings";

/** localStorage key for the persisted interface language. */
export const LOCALE_KEY = "nexora.locale";

function readStoredLocale(): Locale {
  try {
    const raw = window.localStorage.getItem(LOCALE_KEY);
    return isLocale(raw) ? raw : "en";
  } catch {
    return "en";
  }
}

function persistLocale(locale: Locale): void {
  try {
    window.localStorage.setItem(LOCALE_KEY, locale);
  } catch {
    // Best-effort: a rejected write only means the choice may not survive.
  }
}

export interface LocaleStore {
  locale: Locale;
  setLocale: (locale: Locale) => void;
}

const LocaleContext = createContext<LocaleStore | null>(null);

export function LocaleProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>(() => readStoredLocale());

  // Keep the module-level locale (non-React call sites: format.ts, hook
  // fallbacks, label helpers) and <html lang> in sync with React state.
  // The effect below is the backstop (covers the initial mount); setLocale
  // also syncs synchronously so module-locale readers never observe the
  // previous language between the state update and the effect.
  useEffect(() => {
    setCurrentLocale(locale);
    document.documentElement.lang = locale;
  }, [locale]);

  const setLocale = useCallback((next: Locale) => {
    if (!isLocale(next)) return;
    persistLocale(next);
    setCurrentLocale(next);
    setLocaleState(next);
  }, []);

  const value = useMemo(() => ({ locale, setLocale }), [locale, setLocale]);
  return createElement(LocaleContext.Provider, { value }, children);
}

export function useLocale(): LocaleStore {
  const store = useContext(LocaleContext);
  // Outside a provider (never in the app tree — App always wraps) degrade
  // to the module locale with a best-effort setter instead of crashing.
  if (!store) {
    return {
      locale: getLocale(),
      setLocale: (next: Locale) => {
        if (!isLocale(next)) return;
        persistLocale(next);
        setCurrentLocale(next);
      },
    };
  }
  return store;
}

type Vars = Record<string, string | number>;

export interface Strings {
  locale: Locale;
  /** Translate a catalog key (missing RU key falls back to EN, never blank). */
  t: (key: StringKey, vars?: Vars) => string;
  /** Plural-aware translate (RU one/few/many rules). */
  tp: (key: PluralKey, count: number, vars?: Vars) => string;
}

/** Bound `tr`/`tp` for the active locale (re-renders on language switch). */
export function useStrings(): Strings {
  const { locale } = useLocale();
  return useMemo<Strings>(
    () => ({
      locale,
      t: (key, vars) => tr(locale, key, vars),
      tp: (key, count, vars) => tp(locale, key, count, vars),
    }),
    [locale],
  );
}
