//! Provenance badge showing where a layered setting's effective value came
//! from (`Session` | `Project` | `Personal` | `Default`) and whether an
//! untrusted project `.nexora/` override was blocked by security policy.

import type { ResolvedSettingEntry, SettingSource } from "../../lib/tauri";
import type { StringKey } from "../../lib/strings";
import { useStrings } from "../../lib/useLocale";

const SOURCE_KEY: Record<SettingSource, StringKey> = {
  session: "settings.sourceSession",
  project: "settings.sourceProject",
  personal: "settings.sourcePersonal",
  default: "settings.sourceDefault",
};

export interface SettingSourceBadgeProps {
  entry?: ResolvedSettingEntry | null;
}

export default function SettingSourceBadge({ entry }: SettingSourceBadgeProps) {
  const { t } = useStrings();
  if (!entry) return null;
  return (
    <span className="nex-setting-provenance">
      <span className="nex-setting-source-badge" data-source={entry.source}>
        {t(SOURCE_KEY[entry.source])}
      </span>
      {entry.blocked_project_value && (
        <span
          className="nex-setting-source-badge nex-setting-source-blocked"
          title={entry.blocked_reason ?? t("settings.sourceBlocked")}
        >
          {t("settings.sourceBlocked")}
        </span>
      )}
    </span>
  );
}
