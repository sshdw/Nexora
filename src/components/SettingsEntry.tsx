import M3RailItem from "./M3RailItem";
import { SettingsIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface SettingsEntryProps {
  onClick?: () => void;
}

// Navigation entry point for the Settings view (Phase 10.3.2: functional
// provider / model / credential management in the panel it opens). Rendered
// on the shared M3RailItem row primitive (M3E rail canon: pill + tone step
// when active, filled-active/outlined-inactive icon contract).
export default function SettingsEntry({ onClick }: SettingsEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem label={t("nav.settings")} icon={<SettingsIcon />} onClick={onClick} />
  );
}
