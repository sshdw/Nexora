import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles/tokens.css";
import "./styles/motion.css";
import "./styles/theme.css";
import "./styles.css";
import "./styles/components.css";
import "./styles/conversation.css";
import "./styles/settings.css";
import "./styles/prompts.css";
import "./styles/importExport.css";

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
