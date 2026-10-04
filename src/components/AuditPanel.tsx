//! Code audit panel: read-only static analysis of the workspace repository.
//!
//! Presentational over the single `repo_audit` IPC wrapper: one batch round
//! trip returns fixed-vocabulary findings (grouped here by kind with
//! counts), scan accounting, and skip notices. Manual Run only — mounting
//! never scans, and there is no watching or live re-audit. Findings never
//! modify code: rows are copy/read-only text (no auto-fix path exists in
//! this slice).
//!
//! Display rules (secret-free): kinds, severities, and skip reasons render
//! as fixed-vocabulary catalog labels (unknown tokens echo defensively);
//! rows show the workspace-relative `path:line` plus the capped backend
//! excerpt (at most 3 lines — never full files); counts interpolate through
//! the catalog. The panel never animates on entry (instant render under
//! reduced motion); all visuals ride the shared panel/tag/terminal
//! primitives — zero new CSS, zero raw values.

import { useCallback, useState } from "react";

import {
  repoAudit,
  type AuditFinding,
  type CommandError,
  type RepoAuditReport,
} from "../lib/tauri";
import { useStrings, type Strings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";

export interface AuditPanelProps {
  onClose: () => void;
}

/** Finding kinds in panel group order (mirrors the backend `FINDING_KINDS`
 * vocabulary — the label switch below covers exactly these tokens). */
const KIND_ORDER = [
  "dead-code-candidate",
  "unwrap-hotspot",
  "todo-debt",
  "oversized-file",
  "oversized-function",
  "missing-docs",
  "error-swallowed",
  "unchecked-result",
  "suspicious-clone",
] as const;

function toMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).message === "string"
  ) {
    return (error as CommandError).message;
  }
  if (error instanceof Error) return error.message;
  return String(error);
}

/** Fixed-vocabulary kind label; unknown tokens echo defensively (the backend
 * only sends its fixed set, so this branch is defensive). */
function kindLabel(kind: string, t: Strings["t"]): string {
  switch (kind) {
    case "dead-code-candidate":
      return t("audit.kindDeadCode");
    case "unwrap-hotspot":
      return t("audit.kindUnwrap");
    case "todo-debt":
      return t("audit.kindTodo");
    case "oversized-file":
      return t("audit.kindLargeFile");
    case "oversized-function":
      return t("audit.kindLargeFn");
    case "missing-docs":
      return t("audit.kindMissingDocs");
    case "error-swallowed":
      return t("audit.kindSwallowed");
    case "unchecked-result":
      return t("audit.kindUnchecked");
    case "suspicious-clone":
      return t("audit.kindClone");
    default:
      return kind;
  }
}

/** Fixed-vocabulary severity label; unknown tokens echo defensively. */
function severityLabel(severity: string, t: Strings["t"]): string {
  switch (severity) {
    case "warning":
      return t("audit.sevWarning");
    case "info":
      return t("audit.sevInfo");
    default:
      return severity;
  }
}

/** Fixed-vocabulary skip-reason label; unknown tokens echo defensively. */
function skipReasonLabel(reason: string, t: Strings["t"]): string {
  switch (reason) {
    case "too-large":
      return t("audit.skipTooLarge");
    case "unreadable":
      return t("audit.skipUnreadable");
    case "file-cap":
      return t("audit.skipFileCap");
    default:
      return reason;
  }
}

function FindingRow({ finding, t }: { finding: AuditFinding; t: Strings["t"] }) {
  const location = `${finding.path}:${finding.line}`;
  return (
    <li className="nex-vcs-file-row">
      <div>
        <span className="nex-tag nex-tag-mono">{severityLabel(finding.severity, t)}</span>{" "}
        <span className="nex-tag nex-tag-mono" title={location}>
          {location}
        </span>
      </div>
      <pre className="nex-agent-terminal-stdout">{finding.excerpt}</pre>
    </li>
  );
}

export default function AuditPanel({ onClose }: AuditPanelProps) {
  const { t } = useStrings();
  const [running, setRunning] = useState(false);
  const [report, setReport] = useState<RepoAuditReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [kindFilter, setKindFilter] = useState<string>("all");

  // Synchronous in-flight guard: rapid Run presses collapse into the active
  // scan instead of stacking backend walks.
  const run = useCallback(async () => {
    if (running) return;
    setRunning(true);
    setError(null);
    try {
      setReport(await repoAudit());
    } catch (err) {
      setError(toMessage(err));
    } finally {
      setRunning(false);
    }
  }, [running]);

  const visibleKinds =
    kindFilter === "all"
      ? [...KIND_ORDER]
      : KIND_ORDER.filter((kind) => kind === kindFilter);
  const visibleFindings = (kind: string): AuditFinding[] =>
    (report?.findings ?? []).filter((finding) => finding.kind === kind);
  const visibleTotal = visibleKinds.reduce(
    (sum, kind) => sum + visibleFindings(kind).length,
    0,
  );

  return (
    <div className="nex-vcs" role="group" aria-label={t("audit.group")}>
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">{t("audit.title")}</h2>
          <p className="nex-vcs-subtitle">{t("audit.subtitle")}</p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button variant="quiet" onClick={() => void run()} disabled={running}>
            {running ? t("audit.running") : report ? t("audit.rerun") : t("audit.run")}
          </M3Button>
          <M3Button variant="quiet" onClick={onClose}>
            {t("common.backToConversations")}
          </M3Button>
        </div>
      </header>

      <div className="nex-vcs-body">
        <p className="nex-vcs-notice" role="note">
          {t("audit.limitsNote")}
        </p>
        {error && (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {error}
          </div>
        )}
        {running && <M3LoadingIndicator label={t("audit.running")} />}
        {!running && !report && !error && (
          <p className="nex-agent-empty">{t("audit.emptyText")}</p>
        )}
        {!running && report && (
          <>
            <p className="nex-vcs-notice" role="note">
              {t("audit.scanned", { n: report.files_scanned })} ·{" "}
              {t("audit.skipped", { n: report.files_skipped })}
            </p>
            <label className="nex-vcs-notice" htmlFor="nex-audit-kind">
              {t("audit.kindFilter")}
            </label>
            <select
              id="nex-audit-kind"
              className="nex-input"
              value={kindFilter}
              onChange={(event) => setKindFilter(event.target.value)}
            >
              <option value="all">{t("audit.kindAll")}</option>
              {KIND_ORDER.map((kind) => (
                <option key={kind} value={kind}>
                  {kindLabel(kind, t)}
                </option>
              ))}
            </select>
            {visibleTotal === 0 ? (
              <p className="nex-agent-empty">{t("audit.noFindingsText")}</p>
            ) : (
              visibleKinds.map((kind) => {
                const rows = visibleFindings(kind);
                if (rows.length === 0) return null;
                return (
                  <section
                    key={kind}
                    className="nex-vcs-section"
                    aria-label={t("audit.groupCount", {
                      label: kindLabel(kind, t),
                      n: rows.length,
                    })}
                  >
                    <h3 className="nex-vcs-section-title">
                      {t("audit.groupCount", { label: kindLabel(kind, t), n: rows.length })}
                    </h3>
                    <ul className="nex-vcs-file-list">
                      {rows.map((finding) => (
                        <FindingRow
                          key={`${finding.path}:${finding.line}:${finding.kind}`}
                          finding={finding}
                          t={t}
                        />
                      ))}
                    </ul>
                  </section>
                );
              })
            )}
            {report.findings_overflow > 0 && (
              <p className="nex-vcs-notice" role="note">
                {t("audit.moreFindings", { n: report.findings_overflow })}
              </p>
            )}
            {report.skipped.length > 0 && (
              <section className="nex-vcs-section" aria-label={t("audit.skippedTitle")}>
                <h3 className="nex-vcs-section-title">{t("audit.skippedTitle")}</h3>
                <ul className="nex-vcs-file-list">
                  {report.skipped.map((skip) => (
                    <li key={skip.path} className="nex-vcs-file-row">
                      <span className="nex-tag nex-tag-mono">
                        {skipReasonLabel(skip.reason, t)}
                      </span>{" "}
                      <span className="nex-tag nex-tag-mono" title={skip.path}>
                        {skip.path}
                      </span>
                    </li>
                  ))}
                </ul>
                {report.skipped_overflow > 0 && (
                  <p className="nex-vcs-notice" role="note">
                    {t("audit.moreSkipped", { n: report.skipped_overflow })}
                  </p>
                )}
              </section>
            )}
          </>
        )}
      </div>
    </div>
  );
}
