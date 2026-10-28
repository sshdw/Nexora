//! Read-only workspace context row (D3 / R5-09).
//!
//! One line above the composer card: the workspace folder and the branch
//! `gitInfo()` reports. No chevron, no menu, no switching — switching is
//! deferred to a later phase, and a disclosure mark would promise a menu
//! this round does not ship.
//!
//! Data honesty:
//!   - no root → render nothing (not a placeholder dash),
//!   - git fails or reports a detached/unborn branch → the folder only,
//!   - the visible folder text is the basename; the full path is the title.
//!
//! The long-name split is the middle-ellipsis contract: the tail stays
//! fixed and the head is the part CSS truncates, so the end of a folder
//! name is never the part that disappears.

import { useEffect, useState } from "react";

import { gitInfo } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import { BranchIcon, FolderIcon } from "./icons";

export interface WorkspaceContextRowProps {
  /** Effective workspace root, or null when none is open. */
  root: string | null;
}

/** Last path segment, either separator style. Roots arrive as the OS
 *  wrote them (`C:\src\app`, `/home/me/app`). */
export function workspaceFolderName(root: string): string {
  const trimmed = root.replace(/[\\/]+$/, "");
  if (trimmed === "") return root;
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return cut === -1 ? trimmed : trimmed.slice(cut + 1);
}

/** Split a long name into the part CSS may truncate and a fixed tail.
 *  A short name stays whole (no empty tail element is rendered). */
export function splitForMiddleEllipsis(
  name: string,
  tailLength = 10,
): [head: string, tail: string] {
  const chars = [...name];
  if (chars.length <= tailLength * 2) return [name, ""];
  return [
    chars.slice(0, chars.length - tailLength).join(""),
    chars.slice(-tailLength).join(""),
  ];
}

export default function WorkspaceContextRow({ root }: WorkspaceContextRowProps) {
  const { t } = useStrings();
  const [branch, setBranch] = useState<string | null>(null);

  useEffect(() => {
    if (root === null) {
      setBranch(null);
      return;
    }
    let cancelled = false;
    setBranch(null);
    gitInfo()
      .then((info) => {
        if (!cancelled) setBranch(info.branch);
      })
      .catch(() => {
        if (!cancelled) setBranch(null);
      });
    return () => {
      cancelled = true;
    };
  }, [root]);

  if (root === null) return null;

  const name = workspaceFolderName(root);
  const [head, tail] = splitForMiddleEllipsis(name);

  return (
    <div
      className="nex-composer-context"
      role="group"
      aria-label={t("conv.workspaceContextAria")}
    >
      <span className="nex-composer-context-item">
        <FolderIcon size={16} aria-hidden="true" />
        <span className="nex-composer-context-name" title={root}>
          <span className="nex-composer-context-head">{head}</span>
          {tail !== "" && <span className="nex-composer-context-tail">{tail}</span>}
        </span>
      </span>
      {branch !== null && branch !== "" && (
        <>
          <span className="nex-composer-context-sep" aria-hidden="true">
            ·
          </span>
          <span className="nex-composer-context-item">
            <BranchIcon size={16} aria-hidden="true" />
            <span className="nex-composer-context-branch" title={branch}>
              {branch}
            </span>
          </span>
        </>
      )}
    </div>
  );
}
