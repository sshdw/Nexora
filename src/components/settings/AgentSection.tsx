//! Agent section: autonomy mode + run cost limit (AC-208 split, per the
//! review §3.1 plan — JSX moved verbatim from `SettingsView.tsx`). The
//! spend draft state stays in the parent so a section round trip preserves
//! it exactly as before the split.

import type { AutonomyStore } from "../../lib/useAutonomy";
import type { SpendLimitStore } from "../../lib/useSpendLimit";
import { useStrings } from "../../lib/useLocale";
import M3SegmentedGroup from "../M3SegmentedGroup";
import { AUTONOMY_LABELS, AUTONOMY_MODES } from "./model";

export interface AgentSectionProps {
  autonomy: AutonomyStore;
  /** Per-run spend guard store (its error renders under the field). */
  spendLimit: SpendLimitStore;
  /** Persisted limit as displayed (draft when the parent holds one). */
  spendValue: string;
  onSpendDraftChange: (value: string) => void;
  onSpendCommit: () => void;
  onAutonomyChange: (mode: AutonomyStore["mode"]) => void;
}

export default function AgentSection({
  autonomy,
  spendLimit,
  spendValue,
  onSpendDraftChange,
  onSpendCommit,
  onAutonomyChange,
}: AgentSectionProps) {
  const { t } = useStrings();
  return (
    <section className="nex-settings-section" aria-labelledby="agent-heading">
      <h3 id="agent-heading" className="nex-settings-heading">
        {t("settings.sectionAgent")}
      </h3>
      <p className="nex-settings-hint">
        {t("settings.agentHint")}
      </p>
      <div className="nex-settings-field">
        <span className="nex-settings-label" id="autonomy-label">
          {t("settings.autonomyLabel")}
        </span>
        <M3SegmentedGroup
          labelledBy="autonomy-label"
          value={autonomy.mode}
          onChange={(mode) => void onAutonomyChange(mode)}
          options={AUTONOMY_MODES.map((mode) => ({
            value: mode,
            label: t(AUTONOMY_LABELS[mode]),
          }))}
        />
        <p className="nex-settings-hint">
          {t("settings.autonomyHint")}
        </p>
        {autonomy.error && (
          <p className="nex-settings-error nex-fade-in" role="alert">
            {autonomy.error}
          </p>
        )}
      </div>

      <div className="nex-settings-field">
        <label className="nex-settings-label" htmlFor="spend-limit">
          {t("settings.spendLabel")}
        </label>
        <input
          id="spend-limit"
          className="nex-input"
          type="text"
          value={spendValue}
          placeholder={t("settings.spendPh")}
          onChange={(event) => onSpendDraftChange(event.target.value)}
          onBlur={onSpendCommit}
          onKeyDown={(event) => {
            // shortcut:settings.spend-commit.
            if (event.key === "Enter") onSpendCommit();
          }}
        />
        <p className="nex-settings-hint">
          {t("settings.spendHint")}
        </p>
        {spendLimit.error && (
          <p className="nex-settings-error nex-fade-in" role="alert">
            {spendLimit.error.message}
          </p>
        )}
      </div>
    </section>
  );
}
