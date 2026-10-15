import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
// Type payloads, NOT stylesheet entries. These two imports emit
// only `@font-face` registrations (no selectors, no cascade
// participants), so they cannot outrank a utility the way an
// unlayered sheet would — the "exactly one stylesheet entry"
// gate in src/theme.test.ts still holds, by construction (it
// matches only `*.css` import paths, and these are bare
// specifiers). Owner-approved freeze exception per NEXORA-THEME.md
// §3 (#235): `@fontsource-variable/inter` (variable, all seven
// subsets incl. cyrillic in the one index.css) + `@fontsource/
// cascadia-code` (400). The stacks live in src/index.css
// (`--font-sans` / `--font-mono`).
import "@fontsource-variable/inter";
import "@fontsource/cascadia-code";
// The ONLY stylesheet import in the app (#218). The Tailwind/shadcn
// theme layers AND every merged M3E sheet are pulled in from here, in
// one place, each with an explicit cascade layer — see the CASCADE note
// in src/index.css. Adding a second CSS import here would land
// unlayered, and unlayered normal declarations outrank every Tailwind
// utility, which is the bug the single entry exists to prevent.
import "./index.css";

// DEV-ONLY visual-QA hook: with ?mock in the URL (dev builds only), install
// the in-memory IPC stand-in from src/lib/mockBackend.ts before mounting so
// the real UI renders against representative data in a plain browser.
// Production builds dead-code-eliminate this branch entirely.
async function bootstrap(): Promise<void> {
  if (import.meta.env.DEV && new URLSearchParams(window.location.search).has("mock")) {
    await import("./lib/mockBackend");
  }
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void bootstrap();
