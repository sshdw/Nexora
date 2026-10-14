import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
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
