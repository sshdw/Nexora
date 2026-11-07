//! System & diagnostics section: the secret-free bundle (copyable JSON),
//! the read-only update check (button only — never automatic), and the
//! user-initiated pre-update snapshot (AC-208 split, per the review §3.1
//! plan — moved verbatim from `ActivityHealthPanel.tsx`). Renders backend
//! facts only; the IPC actions arrive as props from the parent's
//! `useActivity` store.

import type {
  DiagnosticsBundle,
  SnapshotInfo,
  UpdateCheck,
} from "../../lib/tauri";
import { formatRelativeTime } from "../../lib/format";
import { useStrings } from "../../lib/useLocale";
import M3Button from "../M3Button";
import M3LoadingIndicator from "../M3LoadingIndicator";
import { formatBytes } from "./format";

export interface SystemSectionProps {
  bundle: DiagnosticsBundle | null;
  bundleError: string | null;
  copied: boolean;
  copyFailed: boolean;
  onCopy: () => void;
  update: UpdateCheck | null;
  updateError: string | null;
  updateBusy: boolean;
  onCheckUpdate: () => void;
  snapshot: SnapshotInfo | null;
  snapshotError: string | null;
  snapshotBusy: boolean;
  onSnapshot: () => void;
}

/** System & diagnostics section: the secret-free bundle (copyable JSON),
 * the read-only update check (button only — never automatic), and the
 * user-initiated pre-update snapshot. Renders backend facts only. */
export default function SystemSection({
  bundle,
  bundleError,
  copied,
  copyFailed,
  onCopy,
  update,
  updateError,
  updateBusy,
  onCheckUpdate,
  snapshot,
  snapshotError,
  snapshotBusy,
  onSnapshot,
}: SystemSectionProps) {
  const { locale, t } = useStrings();
  return (
    <section className="nex-activity-section" aria-label={t("diag.section")}>
      <h3 className="nex-activity-section-title">{t("diag.section")}</h3>
      {bundleError !== null ? (
        <p className="nex-activity-error" role="alert">
          {bundleError}
        </p>
      ) : bundle === null ? (
        <M3LoadingIndicator label={t("diag.loading")} />
      ) : (
        <ul className="nex-activity-list">
          <li className="nex-activity-row">
            <div className="nex-activity-row-main">
              <span className="nex-activity-row-title">
                {t("diag.appVersion")}
              </span>
              <span className="nex-activity-pill">{bundle.app_version}</span>
            </div>
            <div className="nex-activity-row-meta">
              <span>
                {t("diag.platform")}: {bundle.platform_os}/
                {bundle.platform_arch}
              </span>
              <span>
                {t("diag.schema")}:{" "}
                {t("diag.schemaValue", {
                  current: bundle.schema_version,
                  target: bundle.schema_target,
                })}
              </span>
            </div>
          </li>
          <li className="nex-activity-row">
            <div className="nex-activity-row-main">
              <span className="nex-activity-row-title">{t("diag.dbSize")}</span>
              <span className="nex-activity-pill">
                {formatBytes(bundle.db_size_bytes, locale)}
              </span>
            </div>
            <div className="nex-activity-row-meta">
              <span>
                {t("diag.counts")}:{" "}
                {t("diag.countsValue", {
                  conversations: bundle.counts.conversations,
                  messages: bundle.counts.messages,
                  prompts: bundle.counts.prompts,
                  runs: bundle.counts.agent_runs,
                })}
              </span>
              <span>
                {t("diag.fts")}:{" "}
                {bundle.fts_present ? t("diag.ftsOk") : t("diag.ftsMissing")}
              </span>
            </div>
          </li>
          <li className="nex-activity-row">
            <div className="nex-activity-row-main">
              <span className="nex-activity-row-title">
                {t("diag.crashes")}
              </span>
              <span className="nex-activity-pill">
                {t("diag.errorRuns", { n: bundle.error_runs })}
              </span>
            </div>
            {bundle.crashed_runs.length === 0 ? (
              <div className="nex-activity-row-meta">
                <span>{t("diag.noCrashes")}</span>
              </div>
            ) : (
              <div className="nex-activity-row-meta">
                {bundle.crashed_runs.map((run) => (
                  <span key={run.run_id}>
                    {t("diag.crashRow", { id: run.run_id, model: run.model })} ·{" "}
                    <time
                      dateTime={new Date(run.started_at * 1000).toISOString()}
                    >
                      {formatRelativeTime(run.started_at, locale)}
                    </time>
                  </span>
                ))}
              </div>
            )}
          </li>
        </ul>
      )}
      {bundle !== null && (
        <div className="nex-activity-row-meta">
          <M3Button variant="quiet" size="sm" onClick={onCopy}>
            {t("diag.copy")}
          </M3Button>
          {copied && <span>{t("diag.copied")}</span>}
          {copyFailed && <span role="alert">{t("common.copyFailed")}</span>}
          {!copied && !copyFailed && <span>{t("diag.copyHint")}</span>}
        </div>
      )}
      <h3 className="nex-activity-section-title">{t("diag.updateSection")}</h3>
      <div className="nex-activity-row-meta">
        <M3Button
          variant="quiet"
          size="sm"
          onClick={onCheckUpdate}
          disabled={updateBusy}
        >
          {updateBusy ? t("diag.checking") : t("diag.check")}
        </M3Button>
        {updateError !== null ? (
          <span role="alert">{t("diag.updateUnavailable")}</span>
        ) : update === null ? (
          <span>{t("diag.downloadNote")}</span>
        ) : update.latest_tag === null ? (
          <span>
            {t("diag.noRelease", { repo: `${update.owner}/${update.repo}` })}
          </span>
        ) : update.update_available ? (
          <span>
            {t("diag.updateFound", {
              latest: update.latest_tag,
              current: update.current_version,
            })}
          </span>
        ) : (
          <span>{t("diag.upToDate", { version: update.current_version })}</span>
        )}
      </div>
      {update !== null &&
        update.update_available &&
        update.latest_url !== null && (
          <p className="nex-activity-muted">
            {t("diag.downloadNote")} {update.latest_url}
          </p>
        )}
      <h3 className="nex-activity-section-title">
        {t("diag.snapshotSection")}
      </h3>
      <p className="nex-activity-muted">{t("diag.snapshotHint")}</p>
      <div className="nex-activity-row-meta">
        <M3Button
          variant="quiet"
          size="sm"
          onClick={onSnapshot}
          disabled={snapshotBusy}
        >
          {snapshotBusy ? t("diag.snapshotBusy") : t("diag.snapshot")}
        </M3Button>
        {snapshotError !== null && <span role="alert">{snapshotError}</span>}
        {snapshot !== null && (
          <span>
            {t("diag.snapshotOk", {
              file: snapshot.file_name,
              size: formatBytes(snapshot.size_bytes, locale),
            })}
          </span>
        )}
      </div>
    </section>
  );
}
