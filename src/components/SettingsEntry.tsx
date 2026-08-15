import { SettingsIcon } from "./icons";

// Visual navigation entry point only. The full Settings screen is a later
// Phase 10 task; this is the anchored nav control in the sidebar.
export default function SettingsEntry() {
  return (
    <button
      type="button"
      className="nex-settings-entry"
      aria-label="Settings"
      title="Settings"
    >
      <SettingsIcon className="nex-settings-icon" />
      <span>Settings</span>
    </button>
  );
}
