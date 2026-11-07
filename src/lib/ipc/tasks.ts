//! Task-manager IPC area (AC-205): user-defined task lists with the bounded
//! plan → act → verify → report loop, plus the kanban edit locks over the
//! same rows.

import { invoke } from "./core";

// ---- Task manager + autonomous mode ------------------------------------
// User-defined task lists with agent-executable steps, plus the bounded
// plan → act → verify → report loop driving the existing agent run path.
// Payloads stay snake_case (serde structs); command args are camelCase
// (`taskId`, `conversationId`, `maxSteps`), like every other command.

/** One `agent_tasks` row as persisted (v8 migration). */
export interface AgentTask {
  id: number;
  title: string;
  description: string | null;
  status: "pending" | "running" | "completed" | "failed" | "cancelled";
  conversation_id: number | null;
  provider: string | null;
  model: string | null;
  max_steps: number;
  current_step: number;
  total_steps: number;
  report: string | null;
  run_id: number | null;
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
}

/** One `agent_task_steps` row as persisted (v8 migration). */
export interface AgentTaskStep {
  id: number;
  task_id: number;
  seq: number;
  title: string;
  status:
    "pending" | "running" | "completed" | "failed" | "skipped" | "cancelled";
  result: string | null;
  run_id: number | null;
  started_at: number; // seconds since unix epoch
  finished_at: number | null;
}

/** One `agent-task-event` frame: secret-free lifecycle (ids + fixed
 * vocabulary only — results and reports reload through the list commands). */
export type AgentTaskEventPayload =
  | { type: "started"; task_id: number }
  | { type: "step_started"; task_id: number; seq: number }
  | { type: "step_finished"; task_id: number; seq: number; status: string }
  | { type: "finished"; task_id: number; status: string };

/** Create a task with its ordered steps. A backing `Task: <title>`
 * conversation is created when `conversationId` is `null`. Returns the
 * schema-assigned task id. */
export function createTask(
  title: string,
  description: string | null,
  conversationId: number | null,
  provider: string | null,
  model: string | null,
  steps: string[],
  maxSteps?: number
): Promise<number> {
  return invoke<number>("create_task", {
    title,
    description: description ?? null,
    conversationId: conversationId ?? null,
    provider: provider ?? null,
    model: model ?? null,
    steps,
    maxSteps: maxSteps ?? null,
  });
}

/** List all tasks, most recently active first. */
export function listTasks(): Promise<AgentTask[]> {
  return invoke<AgentTask[]>("list_tasks");
}

/** List one task's steps, `seq` ascending. */
export function listTaskSteps(taskId: number): Promise<AgentTaskStep[]> {
  return invoke<AgentTaskStep[]>("list_task_steps", { taskId });
}

/** Rename/edit a non-running task. */
export function updateTask(
  taskId: number,
  title: string,
  description: string | null
): Promise<void> {
  return invoke<void>("update_task", {
    taskId,
    title,
    description: description ?? null,
  });
}

/** Delete a non-running task (its steps cascade). */
export function deleteTask(taskId: number): Promise<void> {
  return invoke<void>("delete_task", { taskId });
}

/** Start the autonomous loop for a task. Returns immediately; progress
 * streams via `agent-task-event`. */
export function startTaskRun(taskId: number): Promise<void> {
  return invoke<void>("start_task_run", { taskId });
}

/** Stop the active loop for a task (and abort its in-flight agent run).
 * Returns whether a loop was active. */
export function stopTaskRun(taskId: number): Promise<boolean> {
  return invoke<boolean>("stop_task_run", { taskId });
}

// ---- Kanban edit locks (P2 kanban+locks) -----------------------------------
// Lightweight per-task locks over the SAME task rows: acquire before a
// kanban move, present the token on `moveTask`, release afterwards. Every
// lock carries a deadline (default 120s, range 10..600s — never eternal);
// an expired row is stealable by the next acquirer. Payloads stay snake_case.

/** One `task_locks` row as persisted (v10 migration). */
export interface TaskLock {
  task_id: number;
  holder: string;
  token: string;
  acquired_at: number; // seconds since unix epoch
  expires_at: number; // seconds since unix epoch (deadline, always > acquired_at)
}

/** Acquire the edit lock for a task. Contention with a live lock by another
 * holder rejects with a `conflict`-kind `CommandError` naming the holder
 * and the deadline; an expired row is stolen. */
export function acquireTaskLock(
  taskId: number,
  holder: string,
  ttlSecs?: number
): Promise<TaskLock> {
  return invoke<TaskLock>("acquire_task_lock", {
    taskId,
    holder,
    ttlSecs: ttlSecs ?? null,
  });
}

/** Release the edit lock for a task. The token must match the stored row. */
export function releaseTaskLock(taskId: number, token: string): Promise<void> {
  return invoke<void>("release_task_lock", { taskId, token });
}

/** Extend a live lock's deadline by a fresh TTL. Never resurrects dead
 * locks: a missing row, a mismatch, or an expired row rejects. */
export function heartbeatTaskLock(
  taskId: number,
  token: string,
  ttlSecs?: number
): Promise<TaskLock> {
  return invoke<TaskLock>("heartbeat_task_lock", {
    taskId,
    token,
    ttlSecs: ttlSecs ?? null,
  });
}

/** Move a task to a kanban status (`pending` | `completed` | `failed` |
 * `cancelled`). The token must match the live lock row and be unexpired —
 * stale, mismatched, or missing locks reject and change nothing. */
export function moveTask(
  taskId: number,
  status: "pending" | "completed" | "failed" | "cancelled",
  token: string
): Promise<void> {
  return invoke<void>("move_task", { taskId, status, token });
}
