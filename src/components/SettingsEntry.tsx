import { SettingsIcon } from "./icons";

export interface SettingsEntryProps {
  onClick?: () => void;
}

// Navigation entry point for the Settings view (Phase 10.3.2: functional
// provider / model / credential management in the panel it opens).
export default function SettingsEntry({ onClick }: SettingsEntryProps) {
  return (
    <button
      type="button"
      className="nex-settings-entry"
      aria-label="Settings"
      title="Settings"
      onClick={onClick}
    >
      <SettingsIcon className="nex-settings-icon" />
      <span>Settings</span>
    </button>
  );
}
