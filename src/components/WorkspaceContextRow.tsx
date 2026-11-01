//! Workspace control row (D3 / R5-06 / R6-03).
//!
//! One line above the composer card, and the ONE place a workspace is chosen
//! now that the Dock's Workspace destination and its panel are gone. The row
//! is a popover trigger: the folder name and the branch `gitInfo()` reports
//! are the label, and the menu carries every honest action the store has.
//!
//! The reference (ZCode) shows six rows; Nexora has data for three. Recent is
//! the backend's 5-entry MRU, Workspaces is the registered-root registry, and
//! On This PC / Use Existing… / New Folder… collapse into ONE `Choose folder…`
//! row because all three call the same native picker. `Start from scratch`
//! and the GitHub repository list need capabilities the product does not have
//! (R6-10) and are deliberately absent rather than disabled promises.
//!
//! Data honesty:
//!   - no root → the trigger says so and the menu still offers `Choose folder…`,
//!   - git fails or reports a detached/unborn branch → the folder only,
//!   - the visible folder text is the basename; the full path is the title,
//!   - removing a root unregisters it; the directory itself is never deleted.
//!
//! The long-name split is the middle-ellipsis contract: the tail stays
//! fixed and the head is the part CSS truncates, so the end of a folder
//! name is never the part that disappears.

import { useEffect, useState } from "react";

import { gitInfo } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import type { WorkspaceStore } from "../lib/useWorkspace";
import M3IconButton from "./M3IconButton";
import {
  Popover,
  PopoverContent,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
} from "./ui/popover";
import {
  BranchIcon,
  CheckIcon,
  ChevronDownIcon,
  CloseIcon,
  FolderIcon,
  PlusIcon,
} from "./icons";

export interface WorkspaceContextRowProps {
  /** The app's single workspace store (root, MRU, registry, actions). */
  workspace: WorkspaceStore;
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

/** Recent paths that are not the current root and not already in the
 *  registered-roots registry. A path registered as a workspace must not
 *  appear twice in the same menu; the Recent section simply disappears
 *  when this returns nothing. */
export function recentWithoutRegistered(
  recent: readonly string[],
  roots: readonly string[],
  root: string | null,
  limit = 5,
): string[] {
  const registered = new Set(roots);
  return recent
    .filter((path) => path !== root && !registered.has(path))
    .slice(0, limit);
}

function FolderName({ path }: { path: string }) {
  const name = workspaceFolderName(path);
  const [head, tail] = splitForMiddleEllipsis(name);
  return (
    <span className="nex-composer-context-name" title={path}>
      <span className="nex-composer-context-head">{head}</span>
      {tail !== "" && <span className="nex-composer-context-tail">{tail}</span>}
    </span>
  );
}

export default function WorkspaceContextRow({ workspace }: WorkspaceContextRowProps) {
  const { t } = useStrings();
  const {
    root,
    recent,
    roots,
    loading,
    saving,
    error,
    pickFolder,
    selectRecent,
    removeRoot,
  } = workspace;
  const [branch, setBranch] = useState<string | null>(null);
  const [open, setOpen] = useState(false);

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

  const disabled = loading || saving;
  const recentOthers = recentWithoutRegistered(recent, roots, root);
  const registryOthers = roots.filter((path) => path !== root);

  const chooseFolder = async () => {
    const selected = await pickFolder();
    if (selected !== null) setOpen(false);
  };

  const usePath = async (path: string) => {
    const selected = await selectRecent(path);
    if (selected !== null) setOpen(false);
  };

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="nex-composer-context"
          aria-label={t("conv.workspaceContextAria")}
          title={root ?? t("workspace.none")}
        >
          <FolderIcon size={16} aria-hidden="true" />
          {root === null ? (
            <span className="nex-composer-context-none">{t("workspace.none")}</span>
          ) : (
            <FolderName path={root} />
          )}
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
          <ChevronDownIcon size={16} className="nex-composer-context-chevron" />
        </button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        side="top"
        sideOffset={8}
        collisionPadding={8}
        className="nex-composer-workspace-menu"
      >
        <PopoverHeader>
          <PopoverTitle>{t("workspace.menuTitle")}</PopoverTitle>
        </PopoverHeader>

        {loading && <p className="nex-composer-model-empty">{t("common.loading")}</p>}
        {error !== null && (
          <p className="nex-composer-model-empty" role="alert">
            {error.message}
          </p>
        )}

        {root !== null && (
          <div className="nex-workspace-menu-current">
            <CheckIcon size={16} className="nex-workspace-menu-check" aria-hidden="true" />
            <span className="nex-workspace-menu-text">
              <span className="nex-workspace-menu-name">{workspaceFolderName(root)}</span>
              <span className="nex-workspace-menu-path" title={root}>
                {root}
              </span>
            </span>
          </div>
        )}

        {recentOthers.length > 0 && (
          <section className="nex-workspace-menu-section" aria-label={t("workspace.recent")}>
            <p className="nex-workspace-menu-group">{t("workspace.recent")}</p>
            <ul className="nex-workspace-menu-list">
              {recentOthers.map((path) => (
                <li key={`recent-${path}`}>
                  <button
                    type="button"
                    className="nex-workspace-menu-row"
                    title={path}
                    disabled={disabled}
                    onClick={() => void usePath(path)}
                  >
                    <FolderIcon size={16} aria-hidden="true" />
                    <span className="nex-workspace-menu-text">
                      <span className="nex-workspace-menu-name">
                        {workspaceFolderName(path)}
                      </span>
                      <span className="nex-workspace-menu-path">{path}</span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )}

        {registryOthers.length > 0 && (
          <section className="nex-workspace-menu-section" aria-label={t("workspace.roots")}>
            <p className="nex-workspace-menu-group">{t("workspace.roots")}</p>
            <ul className="nex-workspace-menu-list">
              {registryOthers.map((path) => (
                <li key={`root-${path}`} className="nex-workspace-menu-root">
                  <button
                    type="button"
                    className="nex-workspace-menu-row"
                    title={path}
                    disabled={disabled}
                    onClick={() => void usePath(path)}
                  >
                    <FolderIcon size={16} aria-hidden="true" />
                    <span className="nex-workspace-menu-text">
                      <span className="nex-workspace-menu-name">
                        {workspaceFolderName(path)}
                      </span>
                      <span className="nex-workspace-menu-path">{path}</span>
                    </span>
                  </button>
                  <M3IconButton
                    size="sm"
                    label={t("roots.remove")}
                    title={t("roots.removeTitle")}
                    disabled={disabled}
                    onClick={() => void removeRoot(path)}
                  >
                    <CloseIcon />
                  </M3IconButton>
                </li>
              ))}
            </ul>
          </section>
        )}

        <button
          type="button"
          className="nex-workspace-menu-choose"
          disabled={disabled}
          onClick={() => void chooseFolder()}
        >
          <PlusIcon size={16} aria-hidden="true" />
          <span>{t("workspace.chooseFolder")}</span>
        </button>

        {root === null && (
          <p className="nex-workspace-menu-hint">{t("workspace.openHint")}</p>
        )}
      </PopoverContent>
    </Popover>
  );
}
