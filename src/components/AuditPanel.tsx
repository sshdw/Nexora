//! Code audit panel: read-only static analysis of the workspace repository.
//!
//! Presentational over the single `repo_audit` IPC wrapper: one batch round
//! trip returns fixed-vocabulary findings (grouped here by kind with
//! counts), scan accounting, and skip notices. Manual Run only — mounting
//! never scans, and there is no watching or live re-audit. Findings never
//! modify code: rows are copy/read-only text (no auto-fix path exists in
//! this slice).
//!
//! Test-drafts extension (same panel, second batch command): `testgen_drafts`
//! derives template-generated Rust `#[test]` scaffolds for undocumented or
//! unused public functions from a fresh read-only scan. Drafts are a review
//! buffer — rows carry a copy button plus a per-draft honest-limits note,
//! and nothing is ever written (the user copies manually; applying is a
//! separate slice). TypeScript is out of scope (Rust only).
//!
//! Dependency-inventory extension (same panel, third batch command):
//! `dep_inventory` renders the locked cargo + npm tables (name/version/source
//! rows from `src-tauri/Cargo.lock` and `package-lock.json`, capped
//! server-side). Inventory only — no install/update/remove path exists.
//!
//! Safe-apply extension (same panel, the first WRITE path): `refactor_apply`
//! deletes one confirmed `dead-code-candidate` line range in a Rust file.
//! Every other audit kind refuses; the file must be tracked and clean so the
//! working-tree diff is exactly the apply (revert with `git checkout`), and
//! the panel requires an explicit confirmation tick before sending — the tick
//! IS the approval, and the wrapper always passes `confirmed: true`.
//!
//! Display rules (secret-free): kinds, severities, skip reasons, and dep
//! sources render as fixed-vocabulary catalog labels (unknown tokens echo
//! defensively); rows show the workspace-relative `path:line` plus the capped
//! backend excerpt (at most 3 lines — never full files); counts interpolate
//! through the catalog. The panel never animates on entry (instant render
//! under reduced motion); all visuals ride the shared panel/tag/terminal
//! primitives — zero new CSS, zero raw values.

import { useCallback, useState } from "react";

import {
  depInventory,
  refactorApply,
  repoAudit,
  testgenDrafts,
  type AuditFinding,
  type CommandError,
  type DepEntry,
  type DepInventory,
  type RefactorApplyResult,
  type RepoAuditReport,
  type TestDraft,
  type TestgenReport,
} from "../lib/tauri";
import { useStrings, type Strings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";
import FileIssueDialog, { type FileIssueTarget } from "./FileIssueDialog";

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

/** Fixed-vocabulary dep-source label; unknown tokens echo defensively. */
function depSourceLabel(source: string, t: Strings["t"]): string {
  switch (source) {
    case "crates.io":
      return t("dep.srcCratesIo");
    case "registry":
      return t("dep.srcRegistry");
    case "git":
      return t("dep.srcGit");
    case "local":
      return t("dep.srcLocal");
    case "unknown":
      return t("common.unknown");
    default:
      return source;
  }
}

function DepRow({ entry, t }: { entry: DepEntry; t: Strings["t"] }) {
  return (
    <li className="nex-vcs-file-row">
      <span className="nex-tag nex-tag-mono" title={entry.name}>
        {entry.name}
      </span>{" "}
      <span className="nex-tag nex-tag-mono">{entry.version}</span>{" "}
      <span className="nex-tag nex-tag-mono">{depSourceLabel(entry.source, t)}</span>
    </li>
  );
}

function FindingRow({
  finding,
  t,
  onFileIssue,
}: {
  finding: AuditFinding;
  t: Strings["t"];
  onFileIssue: (finding: AuditFinding) => void;
}) {
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
      <div>
        <M3Button variant="quiet" onClick={() => onFileIssue(finding)}>
          {t("issue.fileIssue")}
        </M3Button>
      </div>
    </li>
  );
}

function DraftRow({
  draft,
  copied,
  onCopy,
  t,
}: {
  draft: TestDraft;
  copied: boolean;
  onCopy: () => void;
  t: Strings["t"];
}) {
  const location = `${draft.path}:${draft.line}`;
  return (
    <li className="nex-vcs-file-row">
      <div>
        <span className="nex-tag nex-tag-mono">{kindLabel(draft.source_kind, t)}</span>{" "}
        <span className="nex-tag nex-tag-mono" title={location}>
          {location}
        </span>
      </div>
      <pre className="nex-agent-terminal-stdout">{draft.code}</pre>
      <p className="nex-vcs-notice" role="note">
        {t("testgen.draftNote")}
      </p>
      <div>
        <M3Button variant="quiet" onClick={onCopy}>
          {copied ? t("testgen.copied") : t("testgen.copy")}
        </M3Button>
      </div>
    </li>
  );
}

export default function AuditPanel({ onClose }: AuditPanelProps) {
  const { t } = useStrings();
  const [running, setRunning] = useState(false);
  const [report, setReport] = useState<RepoAuditReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [kindFilter, setKindFilter] = useState<string>("all");
  const [draftsRunning, setDraftsRunning] = useState(false);
  const [drafts, setDrafts] = useState<TestgenReport | null>(null);
  const [draftsError, setDraftsError] = useState<string | null>(null);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [depsRunning, setDepsRunning] = useState(false);
  const [deps, setDeps] = useState<DepInventory | null>(null);
  const [depsError, setDepsError] = useState<string | null>(null);
  const [applyPath, setApplyPath] = useState("");
  const [applyStart, setApplyStart] = useState("");
  const [applyEnd, setApplyEnd] = useState("");
  const [applyConfirmed, setApplyConfirmed] = useState(false);
  const [applyRunning, setApplyRunning] = useState(false);
  const [applyResult, setApplyResult] = useState<RefactorApplyResult | null>(null);
  const [applyError, setApplyError] = useState<string | null>(null);
  const [issueTarget, setIssueTarget] = useState<FileIssueTarget | null>(null);
  const [issueNotice, setIssueNotice] = useState<string | null>(null);

  // File one finding as a GitHub issue: the prefilled target carries the
  // kind + `path:line` title, the location, and the capped excerpt as the
  // fix detail — the backend assembles the Location/Problem/Fix/Verify body.
  const fileFinding = useCallback((finding: AuditFinding) => {
    const location = `${finding.path}:${finding.line}`;
    setIssueTarget({
      title: `${finding.kind}: ${location}`,
      location,
      detail: finding.excerpt,
      source: "audit",
    });
  }, []);

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

  // Same guard shape for draft generation: one batch round trip at a time.
  const generate = useCallback(async () => {
    if (draftsRunning) return;
    setDraftsRunning(true);
    setDraftsError(null);
    setCopiedKey(null);
    try {
      setDrafts(await testgenDrafts());
    } catch (err) {
      setDraftsError(toMessage(err));
    } finally {
      setDraftsRunning(false);
    }
  }, [draftsRunning]);

  const copyDraft = useCallback(
    async (draft: TestDraft) => {
      const key = `${draft.path}:${draft.line}:${draft.fn_name}`;
      try {
        await navigator.clipboard.writeText(draft.code);
        setCopiedKey(key);
      } catch {
        setDraftsError(t("common.copyFailed"));
      }
    },
    [t],
  );

  // Same guard shape for the dependency inventory: one batch round trip at a
  // time. Inventory only — the backend never writes on this path.
  const loadDeps = useCallback(async () => {
    if (depsRunning) return;
    setDepsRunning(true);
    setDepsError(null);
    try {
      setDeps(await depInventory());
    } catch (err) {
      setDepsError(toMessage(err));
    } finally {
      setDepsRunning(false);
    }
  }, [depsRunning]);

  // Safe apply: one confirmed dead-code removal per press. Frontend
  // validation mirrors the backend guards (non-empty path, positive integer
  // range, explicit tick); the backend re-verifies everything before writing.
  const applyRemoval = useCallback(async () => {
    if (applyRunning) return;
    const path = applyPath.trim();
    const start = Number(applyStart);
    const end = Number(applyEnd);
    if (
      path === "" ||
      !Number.isInteger(start) ||
      !Number.isInteger(end) ||
      start < 1 ||
      end < start
    ) {
      setApplyError(t("refactor.fillAll"));
      return;
    }
    if (!applyConfirmed) {
      setApplyError(t("refactor.needConfirm"));
      return;
    }
    setApplyRunning(true);
    setApplyError(null);
    setApplyResult(null);
    try {
      setApplyResult(await refactorApply(path, start, end, "dead-code-candidate"));
    } catch (err) {
      setApplyError(toMessage(err));
    } finally {
      setApplyRunning(false);
    }
  }, [applyRunning, applyPath, applyStart, applyEnd, applyConfirmed, t]);

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
        {issueNotice && !error && (
          <p className="nex-vcs-notice" role="status">
            {issueNotice}
          </p>
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
            {report.findings_overflow > 0 && (
              <p className="nex-vcs-notice" role="note">
                {t("audit.moreFindings", { n: report.findings_overflow })}
              </p>
            )}
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
                          onFileIssue={fileFinding}
                        />
                      ))}
                    </ul>
                  </section>
                );
              })
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
        <section className="nex-vcs-section" aria-label={t("testgen.sectionTitle")}>
          <h3 className="nex-vcs-section-title">{t("testgen.sectionTitle")}</h3>
          <p className="nex-vcs-notice" role="note">
            {t("testgen.hint")}
          </p>
          <div>
            <M3Button
              variant="quiet"
              onClick={() => void generate()}
              disabled={draftsRunning}
            >
              {draftsRunning
                ? t("testgen.generating")
                : drafts
                  ? t("testgen.regenerate")
                  : t("testgen.generate")}
            </M3Button>
          </div>
          {draftsError && (
            <div className="nex-composer-error nex-fade-in" role="alert">
              {draftsError}
            </div>
          )}
          {draftsRunning && <M3LoadingIndicator label={t("testgen.generating")} />}
          {!draftsRunning && drafts && (
            <>
              <p className="nex-vcs-notice" role="note">
                {t("testgen.targets", {
                  considered: drafts.targets_considered,
                  n: drafts.drafts.length,
                })}
              </p>
              {drafts.drafts_overflow > 0 && (
                <p className="nex-vcs-notice" role="note">
                  {t("testgen.moreDrafts", { n: drafts.drafts_overflow })}
                </p>
              )}
              {drafts.audit_overflow > 0 && (
                <p className="nex-vcs-notice" role="note">
                  {t("testgen.auditOverflow", { n: drafts.audit_overflow })}
                </p>
              )}
              {drafts.drafts.length === 0 ? (
                <p className="nex-agent-empty">{t("testgen.noTargets")}</p>
              ) : (
                <ul className="nex-vcs-file-list">
                  {drafts.drafts.map((draft) => {
                    const key = `${draft.path}:${draft.line}:${draft.fn_name}`;
                    return (
                      <DraftRow
                        key={key}
                        draft={draft}
                        copied={copiedKey === key}
                        onCopy={() => void copyDraft(draft)}
                        t={t}
                      />
                    );
                  })}
                </ul>
              )}
            </>
          )}
        </section>
        <section className="nex-vcs-section" aria-label={t("dep.sectionTitle")}>
          <h3 className="nex-vcs-section-title">{t("dep.sectionTitle")}</h3>
          <p className="nex-vcs-notice" role="note">
            {t("dep.hint")}
          </p>
          <div>
            <M3Button variant="quiet" onClick={() => void loadDeps()} disabled={depsRunning}>
              {depsRunning ? t("dep.loading") : deps ? t("dep.reload") : t("dep.load")}
            </M3Button>
          </div>
          {depsError && (
            <div className="nex-composer-error nex-fade-in" role="alert">
              {depsError}
            </div>
          )}
          {depsRunning && <M3LoadingIndicator label={t("dep.loading")} />}
          {!depsRunning && deps && (
            <>
              {deps.cargo_total === 0 && deps.npm_total === 0 ? (
                <p className="nex-agent-empty">{t("dep.empty")}</p>
              ) : (
                <>
                  <h4 className="nex-vcs-section-title">
                    {t("dep.cargoTitle", { n: deps.cargo_total })}
                  </h4>
                  <ul className="nex-vcs-file-list">
                    {deps.cargo.map((entry) => (
                      <DepRow key={`cargo:${entry.name}`} entry={entry} t={t} />
                    ))}
                  </ul>
                  {deps.cargo_overflow > 0 && (
                    <p className="nex-vcs-notice" role="note">
                      {t("dep.moreDeps", { n: deps.cargo_overflow })}
                    </p>
                  )}
                  <h4 className="nex-vcs-section-title">
                    {t("dep.npmTitle", { n: deps.npm_total })}
                  </h4>
                  <ul className="nex-vcs-file-list">
                    {deps.npm.map((entry) => (
                      <DepRow key={`npm:${entry.name}`} entry={entry} t={t} />
                    ))}
                  </ul>
                  {deps.npm_overflow > 0 && (
                    <p className="nex-vcs-notice" role="note">
                      {t("dep.moreDeps", { n: deps.npm_overflow })}
                    </p>
                  )}
                </>
              )}
            </>
          )}
        </section>
        <section className="nex-vcs-section" aria-label={t("refactor.sectionTitle")}>
          <h3 className="nex-vcs-section-title">{t("refactor.sectionTitle")}</h3>
          <p className="nex-vcs-notice" role="note">
            {t("refactor.hint")}
          </p>
          <p className="nex-vcs-notice" role="note">
            {t("refactor.allowNote")}
          </p>
          <p className="nex-vcs-notice" role="note">
            {t("refactor.breakNote")}
          </p>
          <label className="nex-vcs-notice" htmlFor="nex-refactor-path">
            {t("refactor.pathLabel")}
          </label>
          <input
            id="nex-refactor-path"
            className="nex-input"
            value={applyPath}
            onChange={(event) => setApplyPath(event.target.value)}
            placeholder={t("refactor.pathPh")}
            autoComplete="off"
            spellCheck={false}
          />
          <label className="nex-vcs-notice" htmlFor="nex-refactor-start">
            {t("refactor.startLabel")}
          </label>
          <input
            id="nex-refactor-start"
            className="nex-input"
            value={applyStart}
            onChange={(event) => setApplyStart(event.target.value)}
            inputMode="numeric"
            autoComplete="off"
          />
          <label className="nex-vcs-notice" htmlFor="nex-refactor-end">
            {t("refactor.endLabel")}
          </label>
          <input
            id="nex-refactor-end"
            className="nex-input"
            value={applyEnd}
            onChange={(event) => setApplyEnd(event.target.value)}
            inputMode="numeric"
            autoComplete="off"
          />
          <p className="nex-vcs-notice" role="note">
            {t("refactor.kindLabel")}: {t("audit.kindDeadCode")}
          </p>
          <label className="nex-vcs-notice" htmlFor="nex-refactor-confirm">
            <input
              id="nex-refactor-confirm"
              type="checkbox"
              checked={applyConfirmed}
              onChange={(event) => setApplyConfirmed(event.target.checked)}
            />{" "}
            {t("refactor.confirmLabel")}
          </label>
          <div>
            <M3Button
              variant="quiet"
              onClick={() => void applyRemoval()}
              disabled={applyRunning}
            >
              {applyRunning ? t("refactor.applying") : t("refactor.apply")}
            </M3Button>
          </div>
          {applyError && (
            <div className="nex-composer-error nex-fade-in" role="alert">
              {applyError}
            </div>
          )}
          {applyRunning && <M3LoadingIndicator label={t("refactor.applying")} />}
          {!applyRunning && applyResult && (
            <>
              <p className="nex-vcs-notice" role="note">
                {t("refactor.applied", {
                  n: applyResult.removed_lines,
                  path: applyResult.path,
                  lines: applyResult.file_lines,
                })}
              </p>
              <p className="nex-vcs-notice" role="note">
                {applyResult.verified ? t("refactor.verified") : t("refactor.notVerified")}
              </p>
              <pre className="nex-agent-terminal-stdout">{applyResult.removed_preview}</pre>
            </>
          )}
        </section>
      </div>

      {issueTarget && (
        <FileIssueDialog
          target={issueTarget}
          onFiled={(message) => setIssueNotice(message)}
          onError={(message) => setError(message)}
          onClose={() => setIssueTarget(null)}
        />
      )}
    </div>
  );
}
