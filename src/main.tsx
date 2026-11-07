import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
// ADD-05 root boundary: one render crash anywhere used to unmount the whole
// tree to a blank window (bare render, review 05-ADD §3.5a). The boundary
// degrades to a centred muted screen with a Retry action instead.
import ErrorBoundary from "./components/ErrorBoundary";
// Type payloads, NOT stylesheet entries. These imports emit
// only `@font-face` registrations (no selectors, no cascade
// participants), so they cannot outrank a utility the way an
// unlayered sheet would — the "exactly one stylesheet entry"
// gate in src/theme.test.ts still holds, by construction (it
// matches only `*.css` import paths, and these are bare
// specifiers). Owner-approved freeze exception per NEXORA-THEME.md
// §3 (#235), extended by R4-11: Geist is the primary face,
// `@fontsource-variable/geist` and `@fontsource-variable/
// geist-mono` each ship ONE `index.css` whose five/six
// `@font-face` blocks cover cyrillic + cyrillic-ext + latin +
// latin-ext (+ vietnamese) via unicode-range, so RU and EN both
// render from a single import. `@fontsource-variable/inter`
// (variable, all seven subsets incl. cyrillic) and `@fontsource/
// cascadia-code` (400) stay as their fallbacks. The stacks live
// in src/index.css (`--font-sans` / `--font-mono`).
import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import "@fontsource-variable/inter";
import "@fontsource/cascadia-code";
// The ONLY stylesheet import in the app (#218). The Tailwind/shadcn
// theme layers AND every merged M3E sheet are pulled in from here, in
// one place, each with an explicit cascade layer — see the CASCADE note
// in src/index.css. Adding a second CSS import here would land
// unlayered, and unlayered normal declarations outrank every Tailwind
// utility, which is the bug the single entry exists to prevent.
import "./index.css";
// Adaptive motion level (OD-D10·motion, NEXORA-DESIGN-SYSTEM.md §7.3):
// writes data-motion-level on <html> before the first animated surface
// paints. Tier override blocks live in src/styles/motion.css; storage is
// the device-local `nexora.motion` localStorage key (zero Rust).
import { applyMotionLevel } from "./lib/motionLevel";

// DEV-ONLY visual-QA hook: with ?mock in the URL (dev builds only), install
// the in-memory IPC stand-in from src/lib/mockBackend.ts before mounting so
// the real UI renders against representative data in a plain browser.
// Production builds dead-code-eliminate this branch entirely.
async function bootstrap(): Promise<void> {
  if (
    import.meta.env.DEV &&
    new URLSearchParams(window.location.search).has("mock")
  ) {
    await import("./lib/mockBackend");
  }
  applyMotionLevel();
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <ErrorBoundary root>
        <App />
      </ErrorBoundary>
    </React.StrictMode>
  );
}

void bootstrap();
