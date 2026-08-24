import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles/tokens.css";
import "./styles/motion.css";
import "./styles/theme.css";
import "./styles.css";
import "./styles/conversation.css";
import "./styles/settings.css";
import "./styles/prompts.css";
import "./styles/importExport.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);

