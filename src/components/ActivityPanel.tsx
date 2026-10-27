//! Activity panel — the shell's "what is the agent doing" destination
//! (D-06 / §6.5).
//!
//! Honest by construction. The register is explicit that this surface must
//! never draw an approval or a progress bar it cannot source (G-23: the
//! persisted run rows do not carry `pending_approval` or the current step),
//! so the panel renders exactly one real feed — the agent task queue from
//! `list_tasks` — and says so when the queue is empty. There is no invented
//! "Needs you" section: an approval is raised inside its own run, in the
//! thread that owns it, which is also where the register puts the single
//! Approve control.
//!
//! The queue is fetched on open, not subscribed: tasks change when a user
//! starts one, and the footer link hands the full surface (start/stop/edit)
//! to the Tasks overlay, which owns that lifecycle.

import { useEffect, useState } from "react";

import { listTasks, type AgentTask } from "../lib/tauri";
import { formatRelativeTime } from "../lib/format";
import { useStrings } from "../lib/useLocale";
import M3Button from "./M3Button";

export interface ActivityPanelProps {
  /** Open the full Tasks overlay (the queue's own home). */
  onOpenTasks: () => void;
  /** Open the conversation a task belongs to, when it has one. */
  onOpenConversation: (id: number) => void;
}

export default function ActivityPanel({ onOpenTasks, onOpenConversation }: ActivityPanelProps) {
  const { t, locale } = useStrings();
  const [tasks, setTasks] = useState<AgentTask[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    listTasks()
      .then((rows) => {
        if (!cancelled) setTasks(rows);
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          setTasks([]);
          setError(e instanceof Error ? e.message : t("task.unavailable"));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  const statusWord = (status: AgentTask["status"]): string => {
    switch (status) {
      case "running":
        return t("task.statusRunning");
      case "completed":
        return t("task.statusCompleted");
      case "failed":
        return t("task.statusFailed");
      case "cancelled":
        return t("task.statusCancelled");
      default:
        return t("task.statusPending");
    }
  };

  const running = (tasks ?? []).filter((task) => task.status === "running");
  const queued = (tasks ?? []).filter((task) => task.status === "pending");
  const settled = (tasks ?? [])
    .filter((task) => task.status === "completed" || task.status === "failed" || task.status === "cancelled")
    .slice(0, 4);

  const row = (task: AgentTask, showStatus: boolean) => {
    const meta =
      task.status === "running" && task.total_steps > 0
        ? `${task.current_step}/${task.total_steps}`
        : formatRelativeTime(task.updated_at, locale);
    const body = (
      <>
        <span className="nex-sessions-row-title" title={task.title}>
          {task.title}
        </span>
        <span className="nex-sessions-row-meta">
          {/* The group heading already says "Running" / "Queued", so the row
              repeats the word only in the mixed "Recent" list, where it is
              the one thing that differs between neighbours. */}
          {showStatus && (
            <span className={"nex-activity-status is-" + task.status}>
              {statusWord(task.status)}
            </span>
          )}
          <span className="nex-activity-meta">{meta}</span>
        </span>
      </>
    );
    // A task with a backing conversation opens it; one without has nothing to
    // open, so its row is a statement, not a button that would do nothing.
    return (
      <li key={task.id}>
        {task.conversation_id !== null ? (
          <button
            type="button"
            className="nex-sessions-row"
            onClick={() => onOpenConversation(task.conversation_id as number)}
          >
            {body}
          </button>
        ) : (
          <div className="nex-sessions-row is-static">{body}</div>
        )}
      </li>
    );
  };

  return (
    <div className="nex-activity-panel">
      <div className="nex-sessions-scroll">
        {tasks === null ? (
          <p className="nex-dock-panel-note">{t("common.loading")}</p>
        ) : (
          <>
            {error !== null && (
              <p className="nex-dock-panel-note is-error" role="alert">
                {error}
              </p>
            )}
            {tasks.length === 0 ? (
              <>
                <p className="nex-workspace-name">{t("dock.activityEmpty")}</p>
                <p className="nex-dock-panel-note">{t("dock.activityEmptyText")}</p>
              </>
            ) : (
              <>
                {running.length > 0 && (
                  <>
                    <p className="nex-dock-panel-group">{t("dock.activityRunning")}</p>
                    <ul className="nex-sessions-list">{running.map((task) => row(task, false))}</ul>
                  </>
                )}
                {queued.length > 0 && (
                  <>
                    <p className="nex-dock-panel-group">{t("dock.activityQueued")}</p>
                    <ul className="nex-sessions-list">{queued.map((task) => row(task, false))}</ul>
                  </>
                )}
                {settled.length > 0 && (
                  <>
                    <p className="nex-dock-panel-group">{t("dock.activityRecent")}</p>
                    <ul className="nex-sessions-list">{settled.map((task) => row(task, true))}</ul>
                  </>
                )}
              </>
            )}
          </>
        )}
      </div>

      {/* The queue's own home is one click away and always present — the
          panel is a glance, not a replacement for the Tasks surface. */}
      <div className="nex-dock-panel-footer">
        <M3Button variant="quiet" size="sm" onClick={onOpenTasks}>
          {t("dock.activityOpenTasks")}
        </M3Button>
      </div>
    </div>
  );
}
