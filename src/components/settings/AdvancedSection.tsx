//! Advanced section: feature flags as editable toggles plus the read-only
//! routing-profile / MCP-server / agent-preset summaries, collapsed behind
//! details (AC-208 split, per the review §3.1 plan — JSX moved verbatim from
//! `SettingsView.tsx`; summaries come from `settings/model.ts`). The loaded
//! power-key values stay in the parent (once-per-mount fetch).

import type { Locale } from "../../lib/strings";
import { useStrings } from "../../lib/useLocale";
import {
  FLAG_DEFS,
  MCP_SERVERS_KEY,
  PRESET_KEY,
  ROUTING_AGENT_KEY,
  ROUTING_CHAT_KEY,
  summarizeMcpServers,
  summarizeRoutingProfile,
} from "./model";

export interface AdvancedSectionProps {
  locale: Locale;
  /** Power-key raw values loaded once on mount; `null` = still loading. */
  advancedValues: Record<string, string | null> | null;
  advancedError: string | null;
  flagValues: Record<string, boolean>;
  /** The flag whose toggle is being persisted (disables the whole group). */
  flagsSaving: string | null;
  flagsError: string | null;
  onFlagToggle: (name: string, next: boolean) => void;
}

export default function AdvancedSection({
  locale,
  advancedValues,
  advancedError,
  flagValues,
  flagsSaving,
  flagsError,
  onFlagToggle,
}: AdvancedSectionProps) {
  const { t } = useStrings();
  // Advanced: power keys, demarked and collapsed. Flags are simple booleans
  // (same true/false domain the backend gate accepts) so they get toggles;
  // routing profiles, MCP servers, and the agent preset are complex JSON
  // the UI does not edit — they stay visible but read-only.
  return (
    <section className="nex-settings-section" aria-labelledby="advanced-heading">
      <h3 id="advanced-heading" className="nex-settings-heading">
        {t("settings.sectionAdvanced")}
      </h3>
      <p className="nex-settings-hint">
        {t("settings.advancedHint")}
      </p>
      {advancedError && (
        <p className="nex-settings-error nex-fade-in" role="alert">
          {advancedError}
        </p>
      )}

      <div className="nex-settings-field">
        <span className="nex-settings-label" id="flags-label">
          {t("settings.flagsLabel")}
        </span>
        <p className="nex-settings-hint">
          {t("settings.flagsHint")}
        </p>
        {FLAG_DEFS.map((flag) => (
          <label key={flag.name} className="nex-flag-row" htmlFor={`flag-${flag.name}`}>
            <input
              id={`flag-${flag.name}`}
              type="checkbox"
              checked={flagValues[flag.name] ?? true}
              disabled={flagsSaving !== null}
              onChange={(event) => onFlagToggle(flag.name, event.target.checked)}
              aria-describedby={`flag-${flag.name}-hint`}
            />
            <span>
              <span className="nex-provider-name">{flag.name}</span>{" "}
              <span className="nex-tag">{flag.enforced ? t("settings.enforced") : t("settings.resolvedOnly")}</span>
              <span id={`flag-${flag.name}-hint`} className="nex-settings-hint">
                {" "}{t(flag.descriptionKey)}
              </span>
            </span>
          </label>
        ))}
        {flagsError && (
          <p className="nex-settings-error nex-fade-in" role="alert">
            {flagsError}
          </p>
        )}
      </div>

      <details className="nex-settings-details">
        <summary>{t("settings.routingTitle")}</summary>
        <p className="nex-settings-hint">
          {t("settings.routingHint")}
        </p>
        <div className="nex-settings-field">
          <span className="nex-settings-label">{t("settings.chatProfile")}</span>
          <p className="nex-settings-value">
            {advancedValues === null
              ? t("common.loading")
              : summarizeRoutingProfile(advancedValues[ROUTING_CHAT_KEY] ?? null, locale)}
          </p>
        </div>
        <div className="nex-settings-field">
          <span className="nex-settings-label">{t("settings.agentProfile")}</span>
          <p className="nex-settings-value">
            {advancedValues === null
              ? t("common.loading")
              : summarizeRoutingProfile(advancedValues[ROUTING_AGENT_KEY] ?? null, locale)}
          </p>
        </div>
      </details>

      <details className="nex-settings-details">
        <summary>{t("settings.mcpTitle")}</summary>
        <p className="nex-settings-hint">
          {t("settings.mcpHint")}
        </p>
        <div className="nex-settings-field">
          <span className="nex-settings-label">{t("settings.mcpServers")}</span>
          <p className="nex-settings-value">
            {advancedValues === null
              ? t("common.loading")
              : summarizeMcpServers(advancedValues[MCP_SERVERS_KEY] ?? null, locale)}
          </p>
        </div>
      </details>

      <details className="nex-settings-details">
        <summary>{t("settings.presetTitle")}</summary>
        <p className="nex-settings-hint">
          {t("settings.presetHint")}
        </p>
        <div className="nex-settings-field">
          <span className="nex-settings-label">{t("settings.presetLabel")}</span>
          <p className="nex-settings-value">
            {advancedValues === null
              ? t("common.loading")
              : (advancedValues[PRESET_KEY] ?? t("settings.presetUnset"))}
          </p>
        </div>
      </details>
    </section>
  );
}
