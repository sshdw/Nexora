//! The Health tab body: providers, budget, context pressure, git summary,
//! and flags (AC-208 split, per the review §3.1 plan — moved verbatim from
//! `ActivityHealthPanel.tsx`). All data arrives via props from the parent,
//! which owns the `useActivity` store.

import { useMemo } from "react";

import type {
  ActivityFeed,
  ConversationContextStats,
  FlagsStatus,
  GitInfo,
  ProviderHealth,
  SupportedProvider,
} from "../../lib/tauri";
import { useStrings } from "../../lib/useLocale";
import { formatMicroUsd, healthLabel } from "./format";

export interface HealthTabProps {
  supported: SupportedProvider[];
  health: Record<string, ProviderHealth>;
  healthError: string | null;
  /** The activity feed backs the budget section's totals line. */
  feed: ActivityFeed | null;
  spendLimit: number | null;
  activeConversationId: number | null;
  context: ConversationContextStats | null;
  contextError: string | null;
  git: GitInfo | null;
  gitError: string | null;
  flags: FlagsStatus | null;
  flagsError: string | null;
}

export default function HealthTab({
  supported,
  health,
  healthError,
  feed,
  spendLimit,
  activeConversationId,
  context,
  contextError,
  git,
  gitError,
  flags,
  flagsError,
}: HealthTabProps) {
  const { locale, t, tp: tpn } = useStrings();

  const flagEntries = useMemo(() => {
    if (!flags) return [];
    return Object.entries(flags.flags).sort(([a], [b]) => a.localeCompare(b));
  }, [flags]);

  return (
    <>
      <section className="nex-activity-section" aria-label={t("activity.providersSection")}>
        <h3 className="nex-activity-section-title">{t("activity.providersSection")}</h3>
        {healthError !== null ? (
          <p className="nex-activity-error" role="alert">
            {healthError}
          </p>
        ) : supported.length === 0 ? (
          <p className="nex-activity-muted">{t("activity.noProviders")}</p>
        ) : (
          <ul className="nex-activity-list">
            {supported.map((def) => {
              const probe = health[def.name];
              return (
                <li key={def.name} className="nex-activity-row">
                  <div className="nex-activity-row-main">
                    <span className="nex-activity-row-title">
                      {def.display_name}
                    </span>
                    <span className="nex-activity-pill">
                      {probe ? healthLabel(probe.status, locale) : t("activity.healthUnknown")}
                    </span>
                  </div>
                  <div className="nex-activity-row-meta">
                    <span>
                      {probe
                        ? `${probe.has_configuration ? t("activity.configured") : t("activity.notConfigured")} · ${probe.has_credential ? t("activity.keyStored") : t("activity.noKey")}`
                        : t("activity.noProbe")}
                    </span>
                    <span>{tpn("models", def.models.length)}</span>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </section>
      <section className="nex-activity-section" aria-label={t("activity.budgetSection")}>
        <h3 className="nex-activity-section-title">{t("activity.budgetSection")}</h3>
        {feed === null ? (
          <p className="nex-activity-muted">{t("activity.spendUnavailable")}</p>
        ) : (
          <ul className="nex-activity-list">
            <li className="nex-activity-row">
              <div className="nex-activity-row-main">
                <span className="nex-activity-row-title">{t("activity.allRuns")}</span>
              </div>
              <div className="nex-activity-row-meta">
                <span>
                  {tpn("runs", feed.totals.runs)} · {tpn("steps", feed.totals.total_steps)} ·{" "}
                  {t("activity.totalsTotal", { total: formatMicroUsd(feed.totals.total_spent_micro_usd, locale) })}
                </span>
                <span>
                  {t("activity.limitValue", {
                    limit:
                      spendLimit === null
                        ? t("activity.limitNone")
                        : formatMicroUsd(spendLimit, locale),
                  })}
                </span>
              </div>
            </li>
          </ul>
        )}
      </section>
      <section className="nex-activity-section" aria-label={t("activity.contextSection")}>
        <h3 className="nex-activity-section-title">{t("activity.contextSection")}</h3>
        {activeConversationId === null ? (
          <p className="nex-activity-muted">
            {t("activity.openConv")}
          </p>
        ) : contextError !== null ? (
          <p className="nex-activity-error" role="alert">
            {contextError}
          </p>
        ) : context === null ? (
          <p className="nex-activity-muted">{t("activity.ctxUnavailable")}</p>
        ) : (
          <div className="nex-activity-row">
            <div className="nex-activity-row-main">
              <span className="nex-activity-row-title">{context.title}</span>
              <span className="nex-activity-pill">
                {context.has_token_data
                  ? t("activity.ctxUsed", { n: context.usage_percent })
                  : t("activity.ctxNa")}
              </span>
            </div>
            <div className="nex-activity-row-meta">
              <span>
                {t("ctx.labelMessages")}: {context.message_count} ·{" "}
                {t("ctx.legendTools")}: {context.tool_call_count}
              </span>
              <span>{formatMicroUsd(context.total_cost_micro_usd, locale)}</span>
            </div>
            {context.has_token_data && (
              <div
                className="nex-activity-meter"
                role="progressbar"
                aria-valuenow={context.usage_percent}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-label={t("activity.ctxAria")}
              >
                <div
                  className="nex-activity-meter-fill"
                  style={{ width: `${Math.min(100, context.usage_percent)}%` }}
                />
              </div>
            )}
          </div>
        )}
      </section>
      <section className="nex-activity-section" aria-label={t("activity.gitSection")}>
        <h3 className="nex-activity-section-title">{t("activity.gitSection")}</h3>
        {gitError !== null ? (
          <p className="nex-activity-muted">{gitError}</p>
        ) : git === null ? (
          <p className="nex-activity-muted">{t("activity.gitUnavailable")}</p>
        ) : (
          <div className="nex-activity-row">
            <div className="nex-activity-row-main">
              <span className="nex-activity-row-title">
                {git.branch ?? t("vcs.detachedHead")}
              </span>
              <span className="nex-activity-pill">
                {git.files.length === 0 ? t("activity.clean") : t("activity.dirty")}
              </span>
            </div>
            <div className="nex-activity-row-meta">
              <span>
                {git.files.length === 0
                  ? t("activity.noChanged")
                  : `${tpn("files", git.files.length)}${
                      git.files_overflow > 0 ? ` ${t("activity.filesMore", { n: git.files_overflow })}` : ""
                    }`}
              </span>
              <span>
                {git.commits.length === 0
                  ? t("activity.noCommits")
                  : tpn("commits", git.commits.length)}
              </span>
            </div>
          </div>
        )}
      </section>
      <section className="nex-activity-section" aria-label={t("activity.flagsSection")}>
        <h3 className="nex-activity-section-title">{t("activity.flagsSection")}</h3>
        {flagsError !== null ? (
          <p className="nex-activity-error" role="alert">
            {flagsError}
          </p>
        ) : flagEntries.length === 0 ? (
          <p className="nex-activity-muted">{t("activity.noFlags")}</p>
        ) : (
          <ul className="nex-activity-list">
            {flagEntries.map(([name, flag]) => (
              <li key={name} className="nex-activity-row">
                <div className="nex-activity-row-main">
                  <span className="nex-activity-row-title">{name}</span>
                  <span className="nex-activity-pill">
                    {flag.enabled ? t("common.on") : t("common.off")}
                  </span>
                </div>
                <div className="nex-activity-row-meta">
                  <span>{t("activity.sourcePrefix", { source: flag.source })}</span>
                  <span>{flag.enforced ? t("activity.enforced") : t("activity.notEnforced")}</span>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>
    </>
  );
}
