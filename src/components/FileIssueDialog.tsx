//! Shared GitHub-issue filing dialog: the ConfirmDialog-based confirmation
//! step behind every "File issue" button (debt rows, audit findings,
//! release-check rows).
//!
//! Presentational over the single `create_issue_for_finding` IPC wrapper:
//! the caller supplies the prefilled target (title + optional
//! `path:line` location + optional detail + fixed-vocabulary source), the
//! dialog states what will happen (title, location, origin repo via the
//! user's `gh` CLI — never the full prebuilt body, which the backend
//! assembles from the `Location / Problem / Fix / Verify` template). Success
//! reports through `onFiled` (notice carries the filed issue URL); failure
//! reports through `onError` (the honest backend error, e.g. `gh` missing)
//! and closes — the caller renders both in its own notice/error banner.
//! Auth stays in the user's `gh` CLI — no token ever crosses IPC. Zero new
//! CSS: ConfirmDialog + the caller's shared notice/error primitives only.

import { useCallback, useState } from "react";

import {
  createIssueForFinding,
  type CommandError,
  type IssueSource,
} from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import ConfirmDialog from "./ConfirmDialog";

export interface FileIssueTarget {
  title: string;
  location: string | null;
  detail: string | null;
  source: IssueSource;
}

export interface FileIssueDialogProps {
  target: FileIssueTarget;
  onFiled: (notice: string) => void;
  onError: (message: string) => void;
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

export default function FileIssueDialog({
  target,
  onFiled,
  onError,
  onClose,
}: FileIssueDialogProps) {
  const { t } = useStrings();
  const [busy, setBusy] = useState(false);

  const confirm = useCallback(async () => {
    if (busy) return;
    setBusy(true);
    try {
      const created = await createIssueForFinding(
        target.title,
        target.source,
        true,
        target.location,
        target.detail,
      );
      onFiled(t("issue.created", { n: created.number, url: created.url }));
      onClose();
    } catch (err) {
      onError(toMessage(err));
      onClose();
    } finally {
      setBusy(false);
    }
  }, [busy, target, onFiled, onError, onClose, t]);

  return (
    <ConfirmDialog
      title={t("issue.confirmTitle")}
      body={
        target.location
          ? t("issue.confirmBody", { title: target.title, location: target.location })
          : t("issue.confirmBodyNoLocation", { title: target.title })
      }
      confirmLabel={busy ? t("issue.filing") : t("issue.confirm")}
      cancelLabel={t("common.cancel")}
      busy={busy}
      onConfirm={() => void confirm()}
      onCancel={onClose}
    />
  );
}
