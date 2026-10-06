//! Task-manager store: task list + per-task steps with live reload on the
//! `agent-task-event` stream.
//!
//! Mirrors `useAgentRun` structure: one `listen` on the static event name
//! `"agent-task-event"`, rehydration via `list_tasks` / `list_task_steps`,
//! and persisted rows as the source of truth (live frames only trigger a
//! `reload()` — results and reports always come from the backend).

import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  acquireTaskLock,
  createTask,
  deleteTask,
  heartbeatTaskLock,
  listTaskSteps,
  listTasks,
  moveTask,
  releaseTaskLock,
  startTaskRun,
  stopTaskRun,
  updateTask,
  type AgentTask,
  type AgentTaskEventPayload,
  type AgentTaskStep,
  type TaskLock,
} from "./tauri";

/** One task with its steps as rendered by the TaskPanel. */
export interface TaskView {
  task: AgentTask;
  steps: AgentTaskStep[];
}

export interface TasksStore {
  tasks: TaskView[];
  loading: boolean;
  error: string | null;
  reload: () => Promise<void>;
  create: (
    title: string,
    description: string | null,
    provider: string | null,
    model: string | null,
    steps: string[],
    maxSteps?: number,
  ) => Promise<number | null>;
  update: (taskId: number, title: string, description: string | null) => Promise<boolean>;
  remove: (taskId: number) => Promise<boolean>;
  start: (taskId: number) => Promise<boolean>;
  stop: (taskId: number) => Promise<boolean>;
  /** Acquire the kanban edit lock (surfaces the backend `conflict` error
   * with holder + deadline when another surface holds the task). */
  acquireLock: (taskId: number, holder: string) => Promise<TaskLock | null>;
  /** Release a held lock (best-effort — a wrong token reports failure). */
  releaseLock: (taskId: number, token: string) => Promise<boolean>;
  /** Refresh a live lock's deadline (never resurrects dead locks). */
  heartbeatLock: (taskId: number, token: string) => Promise<TaskLock | null>;
  /** Lock-guarded kanban move: acquire → move → release, then reload. A
   * stale/invalid token or a `conflict` denial reports failure; the message
   * carries the honest backend text (holder + deadline on conflict). */
  move: (
    taskId: number,
    status: "pending" | "completed" | "failed" | "cancelled",
    holder: string,
  ) => Promise<{ ok: boolean; message: string | null }>;
}

function toMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    "message" in error &&
    typeof (error as { message: unknown }).message === "string"
  ) {
    return (error as { message: string }).message;
  }
  if (error instanceof Error) return error.message;
  return String(error);
}

export function useTasks(): TasksStore {
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const fetched: AgentTask[] = await listTasks();
      const views: TaskView[] = await Promise.all(
        fetched.map(async (task) => {
          let steps: AgentTaskStep[] = [];
          try {
            steps = await listTaskSteps(task.id);
          } catch {
            // Best-effort: leave empty, the panel still shows the task row.
            steps = [];
          }
          return { task, steps };
        }),
      );
      setTasks(views);
    } catch (e) {
      setTasks([]);
      setError(toMessage(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Single listen on the static event name: any task frame re-reconciles
  // from the persisted rows (the frames carry ids only, never content).
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;

    const setup = async () => {
      try {
        unlisten = await listen<AgentTaskEventPayload>("agent-task-event", () => {
          void reload();
        });
        if (cancelled && unlisten) {
          unlisten();
          unlisten = null;
        }
      } catch {
        // Listen failed; the store remains in manual-reload mode.
      }
    };

    const cleanupPromise = setup();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
      else void cleanupPromise.then(() => undefined);
    };
  }, [reload]);

  const create = useCallback(
    async (
      title: string,
      description: string | null,
      provider: string | null,
      model: string | null,
      steps: string[],
      maxSteps?: number,
    ): Promise<number | null> => {
      try {
        const id = await createTask(title, description, null, provider, model, steps, maxSteps);
        await reload();
        return id;
      } catch (e) {
        setError(toMessage(e));
        return null;
      }
    },
    [reload],
  );

  const update = useCallback(
    async (taskId: number, title: string, description: string | null): Promise<boolean> => {
      try {
        await updateTask(taskId, title, description);
        await reload();
        return true;
      } catch (e) {
        setError(toMessage(e));
        return false;
      }
    },
    [reload],
  );

  const remove = useCallback(
    async (taskId: number): Promise<boolean> => {
      try {
        await deleteTask(taskId);
        await reload();
        return true;
      } catch (e) {
        setError(toMessage(e));
        return false;
      }
    },
    [reload],
  );

  const start = useCallback(
    async (taskId: number): Promise<boolean> => {
      try {
        await startTaskRun(taskId);
        await reload();
        return true;
      } catch (e) {
        setError(toMessage(e));
        return false;
      }
    },
    [reload],
  );

  const stop = useCallback(
    async (taskId: number): Promise<boolean> => {
      try {
        await stopTaskRun(taskId);
        await reload();
        return true;
      } catch (e) {
        setError(toMessage(e));
        return false;
      }
    },
    [reload],
  );

  const acquireLock = useCallback(
    async (taskId: number, holder: string): Promise<TaskLock | null> => {
      try {
        return await acquireTaskLock(taskId, holder);
      } catch (e) {
        setError(toMessage(e));
        return null;
      }
    },
    [],
  );

  const releaseLock = useCallback(async (taskId: number, token: string): Promise<boolean> => {
    try {
      await releaseTaskLock(taskId, token);
      return true;
    } catch (e) {
      setError(toMessage(e));
      return false;
    }
  }, []);

  const heartbeatLock = useCallback(
    async (taskId: number, token: string): Promise<TaskLock | null> => {
      try {
        return await heartbeatTaskLock(taskId, token);
      } catch (e) {
        setError(toMessage(e));
        return null;
      }
    },
    [],
  );

  const move = useCallback(
    async (
      taskId: number,
      status: "pending" | "completed" | "failed" | "cancelled",
      holder: string,
    ): Promise<{ ok: boolean; message: string | null }> => {
      let token: string | null = null;
      try {
        const lock = await acquireTaskLock(taskId, holder);
        token = lock.token;
        await moveTask(taskId, status, lock.token);
        await reload();
        return { ok: true, message: null };
      } catch (e) {
        const message = toMessage(e);
        setError(message);
        return { ok: false, message };
      } finally {
        // Best-effort release: the lock expires on its own (120s default)
        // when the release itself fails, so a dropped surface never holds
        // a task forever. A stolen token's release fails honestly and is
        // swallowed here — the result already carries the move outcome.
        if (token !== null) {
          try {
            await releaseTaskLock(taskId, token);
          } catch {
            // Swallowed: expiry bounds the leftover lock.
          }
        }
      }
    },
    [reload],
  );

  return { tasks, loading, error, reload, create, update, remove, start, stop, acquireLock, releaseLock, heartbeatLock, move };
}
