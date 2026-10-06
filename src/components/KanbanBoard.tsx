//! Kanban board view over the SAME task rows the list view renders
//! (`TaskPanel`): five columns grouped by the persisted `agent_tasks.status`
//! vocabulary (`pending` / `running` / `completed` / `failed` /
//! `cancelled`), with lock-guarded moves between columns.
//!
//! Move protocol (P2 kanban+locks): every move runs acquire → move → release
//! through the `useTasks` store. The backend validates the lock token, so a
//! stale/invalid token changes nothing and a live lock by another surface
//! fails with holder + deadline — the card then shows an honest "locked"
//! notice with a Take-over button (a re-acquire, which steals the lock iff it
//! has expired). Locks always expire (120s default), so no failed surface
//! holds a task forever.
//!
//! The `running` column is read-only: the autonomous loop owns those rows
//! (moves to/from `running` are rejected by the backend — runs start and
//! stop through the run controls). Moves happen through buttons (keyboard
//! operable) and HTML5 drag-and-drop onto a legal column; both ride the same
//! guarded path. Presentational only: rows, results, and reports always come
//! from the backend via the shared store — the board never invents state.
//! Instant render, no entry animation (reduced-motion safe by construction);
//! all visuals ride tokens + the shared tag/button primitives.

import { useCallback, useMemo, useState } from "react";

import type { AgentTask } from "../lib/tauri";
import type { TasksStore, TaskView } from "../lib/useTasks";
import { useStrings, type Strings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";

type ColumnStatus = AgentTask["status"];
type MoveTarget = "pending" | "completed" | "failed" | "cancelled";

const COLUMNS: readonly ColumnStatus[] = ["pending", "running", "completed", "failed", "cancelled"];
const MOVE_TARGETS: readonly MoveTarget[] = ["pending", "completed", "failed", "cancelled"];

/** Fixed-vocabulary column label; unknown values echo defensively. */
function columnLabel(status: ColumnStatus, t: Strings["t"]): string {
  switch (status) {
    case "pending":
      return t("task.statusPending");
    case "running":
      return t("task.statusRunning");
    case "completed":
      return t("task.statusCompleted");
    case "failed":
      return t("task.statusFailed");
    case "cancelled":
      return t("task.statusCancelled");
    default:
      return status;
  }
}

/** A move is legal unless the loop owns either end (`running`). */
function canMove(from: ColumnStatus, to: MoveTarget): boolean {
  return from !== "running" && from !== to;
}

interface DeniedMove {
  message: string;
  target: MoveTarget;
}

export interface KanbanBoardProps {
  store: TasksStore;
}

export default function KanbanBoard({ store }: KanbanBoardProps) {
  const { t } = useStrings();
  // Per-board-instance holder label: two boards are two surfaces, so a
  // contended lock honestly names the other board instance.
  const [holder] = useState(
    () => `kanban-${Math.random().toString(36).slice(2, 8)}`,
  );
  const [busyIds, setBusyIds] = useState<Record<number, boolean>>({});
  const [denied, setDenied] = useState<Record<number, DeniedMove>>({});
  const [dropTarget, setDropTarget] = useState<MoveTarget | null>(null);

  const columns = useMemo(() => {
    const grouped = new Map<ColumnStatus, TaskView[]>();
    for (const status of COLUMNS) grouped.set(status, []);
    for (const view of store.tasks) {
      const bucket = grouped.get(view.task.status);
      if (bucket) bucket.push(view);
      else grouped.set(view.task.status, [view]);
    }
    return grouped;
  }, [store.tasks]);

  const handleMove = useCallback(
    async (view: TaskView, target: MoveTarget) => {
      const taskId = view.task.id;
      setBusyIds((prev) => ({ ...prev, [taskId]: true }));
      try {
        const result = await store.move(taskId, target, holder);
        if (result.ok) {
          setDenied((prev) => {
            if (!(taskId in prev)) return prev;
            const next = { ...prev };
            delete next[taskId];
            return next;
          });
        } else {
          setDenied((prev) => ({
            ...prev,
            [taskId]: { message: result.message ?? "", target },
          }));
        }
      } finally {
        setBusyIds((prev) => {
          if (!(taskId in prev)) return prev;
          const next = { ...prev };
          delete next[taskId];
          return next;
        });
      }
    },
    [holder, store],
  );

  const handleDropOn = useCallback(
    (target: MoveTarget, event: React.DragEvent) => {
      event.preventDefault();
      setDropTarget(null);
      const raw = event.dataTransfer.getData("text/plain");
      const taskId = Number(raw);
      if (!Number.isInteger(taskId)) return;
      const view = store.tasks.find((candidate) => candidate.task.id === taskId);
      if (!view || !canMove(view.task.status, target)) return;
      void handleMove(view, target);
    },
    [handleMove, store.tasks],
  );

  const doneCount = (view: TaskView) => view.steps.filter((s) => s.status === "completed").length;

  return (
    <div className="nex-term-body">
      {store.error && (
        <div className="nex-composer-error nex-fade-in" role="alert">
          {store.error}
        </div>
      )}
      {store.loading && store.tasks.length === 0 ? (
        <M3LoadingIndicator label={t("common.loading")} />
      ) : (
        <div className="nex-kanban" role="group" aria-label={t("kanban.board")}>
          {COLUMNS.map((status) => {
            const cards = columns.get(status) ?? [];
            const isDropColumn = (status as string) !== "running";
            return (
              <section
                key={status}
                className={
                  "nex-kanban-column" + (dropTarget === status ? " is-drop-target" : "")
                }
                aria-label={`${columnLabel(status, t)} — ${cards.length}`}
                onDragOver={
                  isDropColumn
                    ? (event) => {
                        event.preventDefault();
                        setDropTarget(status as MoveTarget);
                      }
                    : undefined
                }
                onDragLeave={
                  isDropColumn ? () => setDropTarget(null) : undefined
                }
                onDrop={
                  isDropColumn
                    ? (event) => handleDropOn(status as MoveTarget, event)
                    : undefined
                }
              >
                <div className="nex-kanban-column-head">
                  <span>{columnLabel(status, t)}</span>
                  <span className="nex-tag nex-tag-mono">{cards.length}</span>
                </div>
                {status === "running" && cards.length > 0 && (
                  <p className="nex-kanban-note">{t("kanban.runningManaged")}</p>
                )}
                {cards.length === 0 ? (
                  <p className="nex-kanban-empty">{t("kanban.emptyColumn")}</p>
                ) : (
                  cards.map((view) => {
                    const busy = busyIds[view.task.id] === true;
                    const block = denied[view.task.id];
                    const draggable = view.task.status !== "running" && !busy;
                    return (
                      <article
                        key={view.task.id}
                        className="nex-kanban-card"
                        aria-label={`${view.task.title} — ${columnLabel(view.task.status, t)}`}
                        draggable={draggable}
                        onDragStart={(event) => {
                          event.dataTransfer.setData("text/plain", String(view.task.id));
                          event.dataTransfer.effectAllowed = "move";
                        }}
                      >
                        <span className="nex-kanban-card-title">{view.task.title}</span>
                        {view.steps.length > 0 && (
                          <span className="nex-kanban-meta">
                            <span className="nex-tag nex-tag-mono">
                              {t("task.progress", {
                                done: doneCount(view),
                                total: view.steps.length,
                              })}
                            </span>
                          </span>
                        )}
                        {view.task.status !== "running" && (
                          <div className="nex-kanban-actions">
                            {MOVE_TARGETS.filter((target) =>
                              canMove(view.task.status, target),
                            ).map((target) => (
                              <M3Button
                                key={target}
                                variant="quiet"
                                size="sm"
                                disabled={busy}
                                title={t("kanban.moveTo", {
                                  status: columnLabel(target, t),
                                })}
                                onClick={() => void handleMove(view, target)}
                              >
                                {t("kanban.moveTo", { status: columnLabel(target, t) })}
                              </M3Button>
                            ))}
                          </div>
                        )}
                        {block && (
                          <div className="nex-kanban-denied" role="alert">
                            <span>{block.message}</span>
                            <M3Button
                              variant="quiet"
                              size="sm"
                              disabled={busy}
                              onClick={() => void handleMove(view, block.target)}
                            >
                              {t("kanban.takeOver")}
                            </M3Button>
                          </div>
                        )}
                      </article>
                    );
                  })
                )}
              </section>
            );
          })}
        </div>
      )}
    </div>
  );
}
