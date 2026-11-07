//! Privacy & telemetry section: the egress inventory (every surface that
//! can leave the machine, with its live state), the local ledger counters
//! (per-kind totals — counts only, never content), the JSON export
//! (copy/download, exactly like the diagnostics bundle), and the
//! user-confirmed wipe (AC-208 split, per the review §3.1 plan — moved
//! verbatim from `ActivityHealthPanel.tsx`). The export JSON is built from
//! the backend payload only — no raw values are invented in the UI; the IPC
//! actions arrive as props from the parent's `useActivity` store.

import { useMemo, useState } from "react";

import type { LedgerExport, PrivacyStatus, WipeResult } from "../../lib/tauri";
import { useStrings } from "../../lib/useLocale";
import M3Button from "../M3Button";
import M3LoadingIndicator from "../M3LoadingIndicator";
import { ledgerKindLabel, surfaceStateLabel, toMessage } from "./format";

export interface PrivacySectionProps {
  status: PrivacyStatus | null;
  error: string | null;
  onReload: () => void;
  /** Fetch the ledger aggregates for copy/download (IPC via the hook). */
  onExport: () => Promise<LedgerExport>;
  /** Confirmed wipe; reloads the status on success (IPC via the hook). */
  onWipe: () => Promise<WipeResult>;
}

export default function PrivacySection({
  status,
  error,
  onReload,
  onExport,
  onWipe,
}: PrivacySectionProps) {
  const { locale, t } = useStrings();
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  const [wipeArmed, setWipeArmed] = useState(false);
  const [wipeBusy, setWipeBusy] = useState(false);
  const [wiped, setWiped] = useState<number | null>(null);
  const [wipeError, setWipeError] = useState<string | null>(null);

  const totals = useMemo(() => {
    const sums = new Map<string, number>();
    for (const row of status?.stats ?? []) {
      sums.set(row.kind, (sums.get(row.kind) ?? 0) + row.count);
    }
    return [...sums.entries()].sort(([a], [b]) => a.localeCompare(b));
  }, [status]);

  const onCopy = async () => {
    try {
      const exported: LedgerExport = await onExport();
      await navigator.clipboard.writeText(JSON.stringify(exported, null, 2));
      setCopied(true);
      setCopyFailed(false);
    } catch {
      setCopied(false);
      setCopyFailed(true);
    }
  };

  const onDownload = async () => {
    try {
      const exported: LedgerExport = await onExport();
      const blob = new Blob([JSON.stringify(exported, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = "nexora-usage-ledger.json";
      anchor.click();
      URL.revokeObjectURL(url);
    } catch {
      setCopyFailed(true);
    }
  };

  const onWipeConfirm = async () => {
    if (!wipeArmed) {
      setWipeArmed(true);
      setWiped(null);
      setWipeError(null);
      return;
    }
    setWipeBusy(true);
    setWipeError(null);
    try {
      // The confirmation is the native OS dialog the wrapper raises (SEC-03):
      // no caller-supplied boolean exists any more.
      const result = await onWipe();
      setWiped(result.deleted_rows);
      setWipeArmed(false);
      onReload();
    } catch (e) {
      setWipeError(toMessage(e));
    } finally {
      setWipeBusy(false);
    }
  };

  return (
    <section className="nex-activity-section" aria-label={t("privacy.section")}>
      <h3 className="nex-activity-section-title">{t("privacy.section")}</h3>
      <p className="nex-activity-muted">{t("privacy.subtitle")}</p>
      {error !== null ? (
        <p className="nex-activity-error" role="alert">
          {error}
        </p>
      ) : status === null ? (
        <M3LoadingIndicator label={t("privacy.loading")} />
      ) : (
        <>
          <h3 className="nex-activity-section-title">
            {t("privacy.surfaces")}
          </h3>
          <ul className="nex-activity-list">
            {status.surfaces.map((surface) => (
              <li key={surface.id} className="nex-activity-row">
                <div className="nex-activity-row-main">
                  <span className="nex-activity-row-title">
                    {surface.title}
                  </span>
                  <span className="nex-activity-pill">
                    {surfaceStateLabel(surface.state, locale)}
                  </span>
                </div>
                <div className="nex-activity-row-meta">
                  <span>{surface.destination}</span>
                  <span>{surface.detail}</span>
                </div>
              </li>
            ))}
          </ul>
          <h3 className="nex-activity-section-title">{t("privacy.ledger")}</h3>
          {totals.length === 0 ? (
            <p className="nex-activity-muted">{t("privacy.emptyLedger")}</p>
          ) : (
            <ul className="nex-activity-list">
              {totals.map(([kind, count]) => (
                <li key={kind} className="nex-activity-row">
                  <div className="nex-activity-row-main">
                    <span className="nex-activity-row-title">
                      {ledgerKindLabel(kind, locale)}
                    </span>
                    <span className="nex-activity-pill">{count}</span>
                  </div>
                </li>
              ))}
            </ul>
          )}
          <div className="nex-activity-row-meta">
            <span>
              {t("privacy.total", {
                n: status.total_events,
                days: status.retention_days,
              })}
            </span>
          </div>
          <div className="nex-activity-row-meta">
            <M3Button variant="quiet" size="sm" onClick={onCopy}>
              {t("privacy.copy")}
            </M3Button>
            <M3Button variant="quiet" size="sm" onClick={onDownload}>
              {t("privacy.download")}
            </M3Button>
            {copied && <span>{t("privacy.copied")}</span>}
            {copyFailed && <span role="alert">{t("common.copyFailed")}</span>}
            {!copied && !copyFailed && <span>{t("privacy.copyHint")}</span>}
          </div>
          <div className="nex-activity-row-meta">
            {wipeArmed ? (
              <M3Button
                variant="quiet"
                size="sm"
                onClick={() => void onWipeConfirm()}
                disabled={wipeBusy}
              >
                {t("privacy.wipeConfirm")}
              </M3Button>
            ) : (
              <M3Button
                variant="quiet"
                size="sm"
                onClick={() => void onWipeConfirm()}
                disabled={wipeBusy}
              >
                {t("privacy.wipe")}
              </M3Button>
            )}
            {wipeArmed && (
              <M3Button
                variant="quiet"
                size="sm"
                onClick={() => setWipeArmed(false)}
              >
                {t("common.cancel")}
              </M3Button>
            )}
            {wiped !== null && !wipeArmed && (
              <span>{t("privacy.wiped", { n: wiped })}</span>
            )}
            {wipeError !== null && <span role="alert">{wipeError}</span>}
            {!wipeArmed && wiped === null && wipeError === null && (
              <span>{t("privacy.wipeHint")}</span>
            )}
          </div>
        </>
      )}
    </section>
  );
}
