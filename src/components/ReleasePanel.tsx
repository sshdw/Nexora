//! Release panel: operator close-the-loop surface — a release-readiness
//! checklist computed live plus issue automation.
//!
//! Presentational over two IPC wrappers: `release_status` (manifest version
//! parity, migration facts, snapshot presence, `gh` probe — read-only,
//! network-free) and the existing `update_check` (latest upstream release —
//! the one network call, loaded independently so an update failure never
//! hides the local signals). Each checklist row maps computed facts to a
//! pass/warn/fail badge — never hand-waved, never hardcoded: versions agree
//! or name the disagreement, migrations pending counts, the snapshot names
//! its file, the update names its tag, `gh` names its version.
//!
//! Non-pass rows (except the `gh` row itself — filing is impossible without
//! the CLI, and its hint already says what to install) carry a "File issue"
//! button opening the shared prefilled confirm dialog (title from the check,
//! location `release/<check>`, detail from the row); the result notice
//! carries the filed issue URL as text (same rule as the update panel's
//! release URL). Display rules: fixed-vocabulary catalog labels, shared
//! panel/tag/notice primitives — zero new CSS, zero raw values. The panel
//! never animates on entry (instant render under reduced motion).

import { useCallback, useEffect, useState } from "react";

import {
  releaseStatus,
  updateCheck,
  type CommandError,
  type ReleaseStatus,
  type UpdateCheck,
} from "../lib/tauri";
import { formatBytes } from "../lib/format";
import { useStrings, type Strings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";
import FileIssueDialog, { type FileIssueTarget } from "./FileIssueDialog";

export interface ReleasePanelProps {
  onClose: () => void;
}

type CheckState = "pass" | "warn" | "fail";

interface ReleaseCheck {
  id: string;
  label: string;
  state: CheckState;
  detail: string;
  /** Filing target for non-pass rows (`null` = no button, e.g. the `gh`
   * row: filing needs the CLI it reports missing). */
  fileIssue: FileIssueTarget | null;
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

/** Fixed-vocabulary badge label; the state machine has exactly three
 * states, so no defensive echo is needed. */
function stateLabel(state: CheckState, t: Strings["t"]): string {
  switch (state) {
    case "pass":
      return t("release.pass");
    case "warn":
      return t("release.warn");
    case "fail":
      return t("release.fail");
  }
}

function unknownVersion(value: string | null, t: Strings["t"]): string {
  return value ?? t("release.unknownVersion");
}

export default function ReleasePanel({ onClose }: ReleasePanelProps) {
  const { t } = useStrings();
  const [status, setStatus] = useState<ReleaseStatus | null>(null);
  const [update, setUpdate] = useState<UpdateCheck | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [issueTarget, setIssueTarget] = useState<FileIssueTarget | null>(null);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    setNotice(null);
    setUpdateError(null);
    try {
      setStatus(await releaseStatus());
    } catch (err) {
      setStatus(null);
      setError(toMessage(err));
    }
    try {
      setUpdate(await updateCheck());
    } catch (err) {
      setUpdate(null);
      setUpdateError(toMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const checks: ReleaseCheck[] = [];
  if (status) {
    const { package_json_version, cargo_version, tauri_conf_version } = status;
    const versionsLine = t("release.versionsLine", {
      a: unknownVersion(package_json_version, t),
      b: unknownVersion(cargo_version, t),
      c: unknownVersion(tauri_conf_version, t),
    });
    if (status.versions_agree && package_json_version) {
      checks.push({
        id: "versions",
        label: t("release.checkVersions"),
        state: "pass",
        detail: `${t("release.versionsAgree", { v: package_json_version })} ${versionsLine}`,
        fileIssue: null,
      });
    } else if (
      package_json_version &&
      cargo_version &&
      tauri_conf_version
    ) {
      checks.push({
        id: "versions",
        label: t("release.checkVersions"),
        state: "fail",
        detail: `${t("release.versionsDiffer")} ${versionsLine}`,
        fileIssue: {
          title: t("release.issueTitle", { check: t("release.checkVersions") }),
          location: "release/versions",
          detail: versionsLine,
          source: "release-check",
        },
      });
    } else {
      checks.push({
        id: "versions",
        label: t("release.checkVersions"),
        state: "warn",
        detail: `${t("release.versionsUnknown")} ${versionsLine}`,
        fileIssue: {
          title: t("release.issueTitle", { check: t("release.checkVersions") }),
          location: "release/versions",
          detail: versionsLine,
          source: "release-check",
        },
      });
    }

    if (status.pending_migrations === 0) {
      checks.push({
        id: "migrations",
        label: t("release.checkMigrations"),
        state: "pass",
        detail: t("release.migrationsCurrent", {
          applied: status.schema_version,
          target: status.schema_target,
        }),
        fileIssue: null,
      });
    } else {
      checks.push({
        id: "migrations",
        label: t("release.checkMigrations"),
        state: "fail",
        detail: t("release.migrationsPending", { n: status.pending_migrations }),
        fileIssue: {
          title: t("release.issueTitle", { check: t("release.checkMigrations") }),
          location: "release/migrations",
          detail: t("release.migrationsPending", { n: status.pending_migrations }),
          source: "release-check",
        },
      });
    }

    if (status.snapshot) {
      const size = formatBytes(status.snapshot.size_bytes) ?? String(status.snapshot.size_bytes);
      checks.push({
        id: "snapshot",
        label: t("release.checkSnapshot"),
        state: "pass",
        detail: t("release.snapshotFound", {
          name: status.snapshot.file_name,
          size,
        }),
        fileIssue: null,
      });
    } else {
      checks.push({
        id: "snapshot",
        label: t("release.checkSnapshot"),
        state: "warn",
        detail: t("release.snapshotMissing"),
        fileIssue: {
          title: t("release.issueTitle", { check: t("release.checkSnapshot") }),
          location: "release/snapshot",
          detail: t("release.snapshotMissing"),
          source: "release-check",
        },
      });
    }

    if (updateError) {
      checks.push({
        id: "update",
        label: t("release.checkUpdate"),
        state: "warn",
        detail: updateError,
        fileIssue: {
          title: t("release.issueTitle", { check: t("release.checkUpdate") }),
          location: "release/update",
          detail: updateError,
          source: "release-check",
        },
      });
    } else if (update) {
      if (update.rate_limited) {
        checks.push({
          id: "update",
          label: t("release.checkUpdate"),
          state: "warn",
          detail: t("release.updateUnknown"),
          fileIssue: {
            title: t("release.issueTitle", { check: t("release.checkUpdate") }),
            location: "release/update",
            detail: t("release.updateUnknown"),
            source: "release-check",
          },
        });
      } else if (update.update_available) {
        const tag = update.latest_tag ?? update.current_version;
        const detail = t("release.updateAvailable", {
          tag,
          url: update.latest_url ?? "",
        });
        checks.push({
          id: "update",
          label: t("release.checkUpdate"),
          state: "warn",
          detail,
          fileIssue: {
            title: t("release.issueTitle", { check: t("release.checkUpdate") }),
            location: "release/update",
            detail,
            source: "release-check",
          },
        });
      } else if (update.latest_tag) {
        checks.push({
          id: "update",
          label: t("release.checkUpdate"),
          state: "pass",
          detail: t("release.updateCurrent", { tag: update.latest_tag }),
          fileIssue: null,
        });
      } else {
        checks.push({
          id: "update",
          label: t("release.checkUpdate"),
          state: "pass",
          detail: t("release.updateNoReleases"),
          fileIssue: null,
        });
      }
    } else {
      checks.push({
        id: "update",
        label: t("release.checkUpdate"),
        state: "warn",
        detail: t("release.updateUnknown"),
        fileIssue: null,
      });
    }

    if (status.gh_available) {
      checks.push({
        id: "gh",
        label: t("release.checkGh"),
        state: "pass",
        detail: t("release.ghReady", {
          v: status.gh_version ?? t("release.unknownVersion"),
        }),
        fileIssue: null,
      });
    } else {
      checks.push({
        id: "gh",
        label: t("release.checkGh"),
        state: "warn",
        detail: t("release.ghMissing"),
        fileIssue: null,
      });
    }
  }

  return (
    <div className="nex-vcs" role="group" aria-label={t("release.group")}>
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">{t("release.title")}</h2>
          <p className="nex-vcs-subtitle">{t("release.subtitle")}</p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button variant="quiet" onClick={() => void reload()} disabled={loading}>
            {loading ? t("release.refreshing") : t("release.refresh")}
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
        {notice && !error && (
          <p className="nex-vcs-notice" role="status">
            {notice}
          </p>
        )}
        {loading && <M3LoadingIndicator label={t("release.loading")} />}
        {!loading && !error && status === null && (
          <p className="nex-agent-empty">{t("release.unavailable")}</p>
        )}
        {!loading && status && (
          <ul className="nex-vcs-file-list">
            {checks.map((check) => {
              const target = check.fileIssue;
              return (
                <li key={check.id} className="nex-vcs-file-row">
                  <div>
                    <span className="nex-tag nex-tag-mono">
                      {stateLabel(check.state, t)}
                    </span>{" "}
                    <span className="nex-tag nex-tag-mono">{check.label}</span>
                  </div>
                  <p className="nex-vcs-notice">{check.detail}</p>
                  {target && (
                    <div>
                      <M3Button variant="quiet" onClick={() => setIssueTarget(target)}>
                        {t("release.fileIssue")}
                      </M3Button>
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>

      {issueTarget && (
        <FileIssueDialog
          target={issueTarget}
          onFiled={(message) => setNotice(message)}
          onError={(message) => setError(message)}
          onClose={() => setIssueTarget(null)}
        />
      )}
    </div>
  );
}
