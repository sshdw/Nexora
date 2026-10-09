//! Debt backlog panel: tracked technical debt — hand-added entries plus
//! idempotent imports of the read-only `repo_audit` scanner findings.
//!
//! Presentational over the five `create_debt_item` / `list_debt_items` /
//! `update_debt_item_status` / `delete_debt_item` / `import_debt_from_audit`
//! IPC wrappers: the list loads from the persisted rows on mount and after
//! every mutation (the backend is the single source of truth — there are no
//! live events on this path). Importing runs the scanners manually and
//! inserts only fresh findings (re-runs insert nothing and never touch
//! triaged rows); rows render severity + status + source filters, a per-row
//! status control, a manual-add form, and delete with confirmation.
//!
//! Display rules (secret-free): sources, severities, and statuses render as
//! fixed-vocabulary catalog labels (unknown tokens echo defensively); rows
//! show the stored `location` (`path:line` for imports) plus the stored note
//! rendered in full with no line clamping (imports carry the capped scanner
//! excerpt — at most 3 lines, never full files — but hand-added notes run up
//! to 4000 chars and render unclamped); counts interpolate through the
//! catalog. The panel never animates
//! on entry (instant render under reduced motion); all visuals ride the
//! shared panel/tag primitives — zero new CSS, zero raw values.

import { useCallback, useEffect, useState } from "react";

import {
  createDebtItem,
  deleteDebtItem,
  importDebtFromAudit,
  listDebtItems,
  updateDebtItemStatus,
  type CommandError,
  type DebtItem,
} from "../lib/tauri";
import { useStrings, type Strings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";
import ConfirmDialog from "./ConfirmDialog";
import FileIssueDialog, { type FileIssueTarget } from "./FileIssueDialog";

export interface DebtPanelProps {
  onClose: () => void;
}

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

/** Fixed-vocabulary severity label; unknown values echo defensively. */
function severityLabel(severity: string, t: Strings["t"]): string {
  switch (severity) {
    case "info":
      return t("debt.sevInfo");
    case "warning":
      return t("debt.sevWarning");
    default:
      return severity;
  }
}

/** Fixed-vocabulary status label; unknown values echo defensively. */
function statusLabel(status: string, t: Strings["t"]): string {
  switch (status) {
    case "open":
      return t("debt.statusOpen");
    case "accepted":
      return t("debt.statusAccepted");
    case "fixed":
      return t("debt.statusFixed");
    case "wontfix":
      return t("debt.statusWontfix");
    default:
      return status;
  }
}

/** Fixed-vocabulary source label; unknown values echo defensively. */
function sourceLabel(source: string, t: Strings["t"]): string {
  switch (source) {
    case "audit":
      return t("debt.sourceAudit");
    case "manual":
      return t("debt.sourceManual");
    default:
      return source;
  }
}

const STATUS_ORDER = ["open", "accepted", "fixed", "wontfix"] as const;

export default function DebtPanel({ onClose }: DebtPanelProps) {
  const { t } = useStrings();
  const [items, setItems] = useState<DebtItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [severityFilter, setSeverityFilter] = useState<string>("all");
  const [statusFilter, setStatusFilter] = useState<string>("all");
  const [importing, setImporting] = useState(false);
  const [importNotice, setImportNotice] = useState<string | null>(null);
  const [formOpen, setFormOpen] = useState(false);
  const [formTitle, setFormTitle] = useState("");
  const [formSeverity, setFormSeverity] = useState<string>("info");
  const [formLocation, setFormLocation] = useState("");
  const [formNote, setFormNote] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [busyId, setBusyId] = useState<number | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<DebtItem | null>(null);
  const [issueTarget, setIssueTarget] = useState<FileIssueTarget | null>(null);
  const [issueNotice, setIssueNotice] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setItems(await listDebtItems());
    } catch (err) {
      setItems([]);
      setError(toMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Same in-flight guard shape as the audit panel: rapid Import presses
  // collapse into the active run instead of stacking backend scans.
  const runImport = useCallback(async () => {
    if (importing) return;
    setImporting(true);
    setError(null);
    setImportNotice(null);
    try {
      const result = await importDebtFromAudit();
      const base =
        result.inserted > 0
          ? t("debt.imported", { n: result.inserted })
          : t("debt.importedNone");
      setImportNotice(
        result.skipped > 0
          ? `${base} ${t("debt.importSkipped", { n: result.skipped })}`
          : base,
      );
      setItems(await listDebtItems());
    } catch (err) {
      setError(toMessage(err));
    } finally {
      setImporting(false);
    }
  }, [importing, t]);

  const changeStatus = useCallback(
    async (item: DebtItem, status: string) => {
      if (busyId !== null || item.status === status) return;
      setBusyId(item.id);
      setError(null);
      try {
        await updateDebtItemStatus(item.id, status);
        setItems(await listDebtItems());
      } catch (err) {
        setError(toMessage(err));
      } finally {
        setBusyId(null);
      }
    },
    [busyId],
  );

  const confirmDelete = useCallback(async () => {
    const target = deleteTarget;
    if (!target || busyId !== null) return;
    setBusyId(target.id);
    setDeleteTarget(null);
    setError(null);
    try {
      await deleteDebtItem(target.id);
      setItems(await listDebtItems());
    } catch (err) {
      setError(toMessage(err));
    } finally {
      setBusyId(null);
    }
  }, [deleteTarget, busyId]);

  const saveManual = useCallback(async () => {
    if (saving) return;
    const title = formTitle.trim();
    if (title === "") {
      setFormError(t("debt.needTitle"));
      return;
    }
    setSaving(true);
    setFormError(null);
    setError(null);
    try {
      await createDebtItem(
        title,
        formSeverity,
        formLocation.trim() === "" ? null : formLocation.trim(),
        formNote.trim() === "" ? null : formNote.trim(),
      );
      setFormOpen(false);
      setFormTitle("");
      setFormSeverity("info");
      setFormLocation("");
      setFormNote("");
      setItems(await listDebtItems());
    } catch (err) {
      setFormError(toMessage(err));
    } finally {
      setSaving(false);
    }
  }, [saving, formTitle, formSeverity, formLocation, formNote, t]);

  const visible = items.filter(
    (item) =>
      (severityFilter === "all" || item.severity === severityFilter) &&
      (statusFilter === "all" || item.status === statusFilter),
  );

  return (
    <div className="nex-vcs" role="group" aria-label={t("debt.group")}>
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">{t("debt.title")}</h2>
          <p className="nex-vcs-subtitle">{t("debt.subtitle")}</p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button
            variant="quiet"
            onClick={() => void runImport()}
            disabled={importing || loading}
          >
            {importing ? t("debt.importing") : t("debt.import")}
          </M3Button>
          <M3Button variant="quiet" onClick={onClose}>
            {t("common.backToConversations")}
          </M3Button>
        </div>
      </header>

      <div className="nex-vcs-body">
        {error && (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {error}
          </div>
        )}
        {importNotice && !error && (
          <p className="nex-vcs-notice" role="status">
            {importNotice}
          </p>
        )}
        {issueNotice && !error && (
          <p className="nex-vcs-notice" role="status">
            {issueNotice}
          </p>
        )}
        {loading && <M3LoadingIndicator label={t("common.loading")} />}
        {!loading && (
          <>
            <p className="nex-vcs-notice" role="note">
              {t("debt.count", { n: visible.length })}
            </p>
            <label className="nex-vcs-notice" htmlFor="nex-debt-severity">
              {t("debt.severityFilter")}
            </label>
            <select
              id="nex-debt-severity"
              className="nex-input"
              value={severityFilter}
              onChange={(event) => setSeverityFilter(event.target.value)}
            >
              <option value="all">{t("debt.severityAll")}</option>
              <option value="info">{t("debt.sevInfo")}</option>
              <option value="warning">{t("debt.sevWarning")}</option>
            </select>
            <label className="nex-vcs-notice" htmlFor="nex-debt-status">
              {t("debt.statusFilter")}
            </label>
            <select
              id="nex-debt-status"
              className="nex-input"
              value={statusFilter}
              onChange={(event) => setStatusFilter(event.target.value)}
            >
              <option value="all">{t("debt.statusAll")}</option>
              {STATUS_ORDER.map((status) => (
                <option key={status} value={status}>
                  {statusLabel(status, t)}
                </option>
              ))}
            </select>
            {items.length === 0 ? (
              <p className="nex-agent-empty">{t("debt.emptyText")}</p>
            ) : visible.length === 0 ? (
              <p className="nex-agent-empty">{t("debt.noMatchText")}</p>
            ) : (
              <ul className="nex-vcs-file-list">
                {visible.map((item) => (
                  <li key={item.id} className="nex-vcs-file-row">
                    <div>
                      <span className="nex-tag nex-tag-mono">
                        {severityLabel(item.severity, t)}
                      </span>{" "}
                      <span className="nex-tag nex-tag-mono">
                        {sourceLabel(item.source, t)}
                      </span>{" "}
                      {item.location && (
                        <span className="nex-tag nex-tag-mono" title={item.location}>
                          {item.location}
                        </span>
                      )}
                    </div>
                    <p className="nex-vcs-notice">{item.title}</p>
                    {item.note && (
                      <pre className="nex-agent-terminal-stdout">{item.note}</pre>
                    )}
                    <div>
                      <label
                        className="nex-vcs-notice"
                        htmlFor={`nex-debt-status-${item.id}`}
                      >
                        {t("debt.statusFilter")}
                      </label>{" "}
                      <select
                        id={`nex-debt-status-${item.id}`}
                        className="nex-input"
                        value={item.status}
                        disabled={busyId === item.id}
                        onChange={(event) => void changeStatus(item, event.target.value)}
                      >
                        {STATUS_ORDER.map((status) => (
                          <option key={status} value={status}>
                            {statusLabel(status, t)}
                          </option>
                        ))}
                      </select>{" "}
                      <M3Button
                        variant="quiet"
                        onClick={() => setDeleteTarget(item)}
                        disabled={busyId === item.id}
                      >
                        {t("debt.delete")}
                      </M3Button>{" "}
                      <M3Button
                        variant="quiet"
                        onClick={() =>
                          setIssueTarget({
                            title: item.title,
                            location: item.location,
                            detail: item.note,
                            source: "debt",
                          })
                        }
                        disabled={busyId === item.id}
                      >
                        {t("issue.fileIssue")}
                      </M3Button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </>
        )}

        <section className="nex-vcs-section" aria-label={t("debt.new")}>
          <h3 className="nex-vcs-section-title">{t("debt.new")}</h3>
          {!formOpen ? (
            <div>
              <M3Button variant="quiet" onClick={() => setFormOpen(true)}>
                {t("debt.create")}
              </M3Button>
            </div>
          ) : (
            <>
              {formError && (
                <div className="nex-composer-error nex-fade-in" role="alert">
                  {formError}
                </div>
              )}
              <label className="nex-vcs-notice" htmlFor="nex-debt-title">
                {t("debt.titleLabel")}
              </label>
              <input
                id="nex-debt-title"
                className="nex-input"
                type="text"
                value={formTitle}
                placeholder={t("debt.titlePh")}
                onChange={(event) => setFormTitle(event.target.value)}
              />
              <label className="nex-vcs-notice" htmlFor="nex-debt-form-severity">
                {t("debt.severityLabel")}
              </label>
              <select
                id="nex-debt-form-severity"
                className="nex-input"
                value={formSeverity}
                onChange={(event) => setFormSeverity(event.target.value)}
              >
                <option value="info">{t("debt.sevInfo")}</option>
                <option value="warning">{t("debt.sevWarning")}</option>
              </select>
              <label className="nex-vcs-notice" htmlFor="nex-debt-location">
                {t("debt.locationLabel")}
              </label>
              <input
                id="nex-debt-location"
                className="nex-input"
                type="text"
                value={formLocation}
                placeholder={t("debt.locationPh")}
                onChange={(event) => setFormLocation(event.target.value)}
              />
              <label className="nex-vcs-notice" htmlFor="nex-debt-note">
                {t("debt.noteLabel")}
              </label>
              <input
                id="nex-debt-note"
                className="nex-input"
                type="text"
                value={formNote}
                placeholder={t("debt.notePh")}
                onChange={(event) => setFormNote(event.target.value)}
              />
              <div>
                <M3Button
                  variant="quiet"
                  onClick={() => void saveManual()}
                  disabled={saving}
                >
                  {t("debt.save")}
                </M3Button>{" "}
                <M3Button
                  variant="quiet"
                  onClick={() => {
                    setFormOpen(false);
                    setFormError(null);
                  }}
                >
                  {t("debt.cancel")}
                </M3Button>
              </div>
            </>
          )}
        </section>
      </div>

      {deleteTarget && (
        <ConfirmDialog
          title={t("debt.deleteTitle")}
          body={t("debt.deleteBody", { title: deleteTarget.title })}
          confirmLabel={t("debt.deleteConfirm")}
          onConfirm={() => void confirmDelete()}
          onCancel={() => setDeleteTarget(null)}
        />
      )}
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
