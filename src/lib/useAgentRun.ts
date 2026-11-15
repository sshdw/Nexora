//! Conversation-owned live presentation and read-only durable reconciliation.
//! Initial metadata stays bounded to 50 runs; old steps and full observations
//! retain their lazy APIs. A serialized forward reader acknowledges only fully
//! processed pages, while generation and request fences isolate async work.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  type AgentRunEventPayload,
  type ApprovalMetadata,
  type AgentStep,
  type DurableRunEvent,
  type FileCheckpoint,
  type PendingQuestionView,
  type PlanDecisionStatus,
  type RunPlan,
  type RunQuestion,
  type StepEventFrame,
  answerRunQuestion,
  clarifyAgentRun,
  getAgentStepObservation,
  inspectRunLive,
  listAgentRuns,
  listAgentSteps,
  listConversationRunEvents,
  listRunCheckpoints,
  listRunPlans,
  listRunQuestions,
  revertFileCheckpoint,
  revertTurnCheckpoints,
} from "./tauri";

/** Bounded rehydration page (AC-102/ADD-08): the newest 50 runs of a
 * conversation load on switch; older history stays unpaged (no load-more
 * affordance in this change — a later cursor page through `beforeRunId` is
 * the extension point). 50 covers any realistic visible thread while keeping
 * the switch-off IPC count flat as histories grow. */
const REHYDRATION_RUN_PAGE = 50;

/** One step as rendered in the accordion (persisted shape + live append).
 * NEX-PERF-003: `observation` is the bounded preview (or the full text when
 * short); the full text of a truncated step loads on demand into
 * `observation_full` and is dropped again on collapse, so the render tree
 * stays bounded no matter how long the run is. */
export interface AgentStepView {
  id: number; // 0 for live-only before rehydration; real id after reload
  step_id: number; // agent_steps.id — the on-demand full-text fetch key (0 for legacy live-only frames)
  run_id: number;
  seq: number;
  kind: string;
  tool_name: string | null;
  arguments: string | null;
  observation: string | null;
  observation_len: number | null;
  observation_truncated: boolean;
  observation_full: string | null;
  observation_loading: boolean;
  observation_error: string | null;
  status: string | null;
  started_at: number | null;
  duration_ms: number | null;
}

/** One run as rendered by `AgentRunSteps` (Task 5.1 §6.3). */
export interface AgentRunView {
  run_id: number;
  conversation_id: number;
  status: string;
  model: string;
  started_at: number;
  finished_at: number | null;
  error: string | null;
  steps: AgentStepView[];
  /** AC-102/ADD-08: false while the run rehydrated WITHOUT its step list
   *  (steps lazy-load through `loadRunSteps` when the run's step list is
   *  shown). Live runs are true from birth — their steps stream in. */
  steps_loaded: boolean;
  /** Persisted `agent_runs.total_steps` (0 for live placeholders until a
   *  reload lands). Lets the run line stay honest while steps are unloaded. */
  total_steps: number | null;
  /** Ephemeral live-streamed assistant text while the run turn is executing. */
  streaming_text?: string;
  /** Ephemeral live-streamed provider-native thinking while the run turn is executing. */
  streaming_thinking?: string;
  /** Retained provider-native thinking emitted during the run (never fabricated). */
  thinking?: string | null;
  /** Persisted or terminal final assistant result for this run. */
  final_content?: string | null;
  /** Optional plan outline content when the run produced or carries a plan. */
  plan_content?: string | null;
  /** True when the run itself is a read-only plan run. */
  is_plan?: boolean;
  /** Persisted decision status for Plan runs (`pending | accepted | rejected | custom_reply`). */
  plan_decision_status?: PlanDecisionStatus | null;
  /** Optional custom reply text recorded when `plan_decision_status === "custom_reply"`. */
  plan_decision_reply?: string | null;
  /** Follow-up implementation run id launched from this Plan run, if any. */
  plan_implemented_run_id?: number | null;
  /** Parent interrupted run id when this run was started via explicit Continue. */
  continued_from_run_id?: number | null;
  /** Backend persisted interrupted flag (preserved alongside normalized status). */
  interrupted?: boolean;
  pending_approval: {
    metadata?: ApprovalMetadata;
    call_id: string;
    name: string;
    arguments: string;
    timeout_secs: number;
    requested_at: number;
  } | null;
  /** Active `ask_user_question` prompt while the run is parked awaiting input. */
  pending_question?: PendingQuestionView | null;
  /** Latest `Clarify now` steering message delivered to the active run. */
  last_steering?: string | null;
  /** Disk-backed pre-mutation file checkpoints for this run, lazy-loaded. */
  checkpoints?: FileCheckpoint[];
  /** True after `listRunCheckpoints` has completed for the current run view. */
  checkpoints_loaded?: boolean;
}

export type RevertOutcome =
  | { status: "reverted" }
  | { status: "conflict"; message: string }
  | { status: "error"; message: string };

function parseRevertError(cause: unknown): RevertOutcome {
  if (typeof cause === "object" && cause !== null) {
    const record = cause as { kind?: unknown; message?: unknown };
    const message =
      typeof record.message === "string"
        ? record.message
        : cause instanceof Error
          ? cause.message
          : String(cause);
    if (record.kind === "conflict") {
      return { status: "conflict", message };
    }
    return { status: "error", message };
  }
  return {
    status: "error",
    message: cause instanceof Error ? cause.message : String(cause),
  };
}

function toStepView(step: AgentStep): AgentStepView {
  return {
    id: step.id,
    step_id: step.id,
    run_id: step.run_id,
    seq: step.seq,
    kind: step.kind,
    tool_name: step.tool_name,
    arguments: step.arguments,
    observation: step.observation,
    observation_len: step.observation_len,
    observation_truncated: step.observation_truncated,
    observation_full: null,
    observation_loading: false,
    observation_error: null,
    status: step.status,
    started_at: step.started_at,
    duration_ms: step.duration_ms,
  };
}

function toStepViewFromFrame(
  runId: number,
  frame: StepEventFrame
): AgentStepView {
  return {
    id: 0,
    step_id: frame.step_id,
    run_id: runId,
    seq: frame.seq,
    kind: frame.kind,
    tool_name: frame.tool_name,
    arguments: frame.arguments,
    observation: frame.observation,
    observation_len: frame.observation_len,
    observation_truncated: frame.observation_truncated,
    observation_full: null,
    observation_loading: false,
    observation_error: null,
    status: frame.status,
    started_at: null,
    duration_ms: frame.duration_ms,
  };
}

export interface AgentRunStore {
  runs: AgentRunView[];
  loading: boolean;
  error: string | null;
  reload: () => Promise<void>;
  /** AC-102/ADD-08: lazy-load one run's persisted step list (bounded
   * previews, NEX-PERF-003). Idempotent — a no-op for live runs (their steps
   * streamed in) and for runs already loaded or in flight. */
  loadRunSteps: (runId: number) => Promise<void>;
  /** Fetch the full observation text of one truncated step (NEX-PERF-003
   * on-demand expand). Sets the step's loading flag, fills
   * `observation_full` on success or `observation_error` on failure. */
  expandStep: (runId: number, seq: number) => Promise<void>;
  /** Drop a step's fetched full text, back to the bounded preview. */
  collapseStep: (runId: number, seq: number) => void;
  /** Lazy-load pre/post-mutation file checkpoints recorded for one run. */
  loadRunCheckpoints: (runId: number) => Promise<void>;
  /** Revert one file checkpoint (`force=true` overrides external conflict). */
  revertCheckpoint: (
    runId: number,
    checkpointId: number,
    force?: boolean
  ) => Promise<RevertOutcome>;
  /** Revert all un-reverted checkpoints recorded during a single turn. */
  revertTurn: (
    runId: number,
    turnIndex: number,
    force?: boolean
  ) => Promise<RevertOutcome>;
  /** Submit the user's answer to a pending `ask_user_question` prompt. */
  answerQuestion: (
    runId: number,
    questionId: number,
    selectedOptions: string[],
    freeText?: string
  ) => Promise<RunQuestion>;
  /** Inject a live `Clarify now` steering message into an active run. */
  clarifyRun: (runId: number, content: string) => Promise<void>;
  /** Synchronously merge a recorded Plan decision into the local run view. */
  applyPlanDecision: (runId: number, plan: RunPlan) => void;
}

/** Merge previews by durable identity first, with seq as the legacy fallback.
 * Existing expanded text belongs to the displayed step, not a stale snapshot. */
function mergeSteps(
  current: AgentStepView[],
  incoming: AgentStepView[]
): AgentStepView[] {
  const result = [...current];
  for (const step of incoming) {
    const index = result.findIndex(
      (old) =>
        (step.step_id > 0 && old.step_id === step.step_id) ||
        old.seq === step.seq
    );
    if (index < 0) result.push(step);
    else if (
      (result[index].step_id === 0 && step.step_id > 0) ||
      (result[index].id === 0 && step.id > 0)
    ) {
      result[index] = { ...result[index], id: step.id, step_id: step.step_id };
    }
  }
  return result.sort((a, b) => a.seq - b.seq);
}

function isActive(status: string): boolean {
  return [
    "running",
    "paused",
    "budget_exhausted",
    "spend_limit_exceeded",
  ].includes(status);
}

function placeholder(runId: number, conversationId: number): AgentRunView {
  return {
    run_id: runId,
    conversation_id: conversationId,
    status: "running",
    model: "",
    started_at: Date.now() / 1000,
    finished_at: null,
    error: null,
    steps: [],
    steps_loaded: false,
    total_steps: null,
    pending_approval: null,
    pending_question: null,
  };
}

interface ConversationSession {
  conversationId: number;
  runs: AgentRunView[];
  request: number;
  revision: Map<number, number>;
  stepsLoading: Set<number>;
  checkpointsLoading: Set<number>;
  observations: Map<string, symbol>;
  cursor: number;
  initialHighWater: number | null;
  historyFloor: number;
  ready: boolean;
  dirty: boolean;
  drain: Promise<void> | null;
}

export function useAgentRun(conversationId: number | null): AgentRunStore {
  const [runs, setRuns] = useState<AgentRunView[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // A fresh object per effect lifetime fences A -> B -> A and unmount, not
  // merely changes of numeric conversation id. All async writes use it.
  const sessionRef = useRef<ConversationSession | null>(null);
  const current = useCallback(
    (session: ConversationSession) => sessionRef.current === session,
    []
  );
  const publish = useCallback(
    (session: ConversationSession, next: AgentRunView[]) => {
      if (sessionRef.current !== session) return;
      session.runs = next;
      setRuns(next);
    },
    []
  );
  const patch = useCallback(
    (
      session: ConversationSession,
      runId: number,
      change: (run: AgentRunView) => AgentRunView
    ) => {
      if (sessionRef.current !== session) return;
      const old = session.runs.find((run) => run.run_id === runId);
      const next = change(old ?? placeholder(runId, session.conversationId));
      if (next === old) return;
      session.revision.set(runId, (session.revision.get(runId) ?? 0) + 1);
      publish(
        session,
        old
          ? session.runs.map((run) => (run.run_id === runId ? next : run))
          : [...session.runs, next]
      );
    },
    [publish]
  );

  const catchUp = useCallback(
    (session: ConversationSession): Promise<void> => {
      if (!current(session)) return Promise.resolve();
      session.dirty = true;
      if (!session.ready) return Promise.resolve();
      if (session.drain) return session.drain;
      // One forward reader per generation. Hints arriving during a read request
      // another pass; their event_id is never an acknowledged cursor.
      const drain = async () => {
        try {
          while (current(session) && session.dirty) {
            session.dirty = false;
            let more = true;
            while (current(session) && more) {
              const after = session.cursor;
              const revisions = new Map(session.revision);
              const page = await listConversationRunEvents(
                session.conversationId,
                after,
                100
              );
              if (!current(session)) return;
              session.initialHighWater ??= page.high_water_event_id;
              const protectedTerminals = new Set(
                session.runs
                  .filter(
                    (run) =>
                      !isActive(run.status) &&
                      (session.revision.get(run.run_id) ?? 0) !==
                        (revisions.get(run.run_id) ?? 0)
                  )
                  .map((run) => run.run_id)
              );
              const seen = new Set<number>();
              for (const event of page.events) {
                if (event.event_id <= after || seen.has(event.event_id))
                  continue;
                seen.add(event.event_id);
                if (
                  event.conversation_id !== session.conversationId ||
                  (event.step &&
                    (event.step.run_id !== event.run_id ||
                      event.step.id !== event.step_id)) ||
                  (event.terminal &&
                    event.terminal.conversation_id !== session.conversationId)
                ) {
                  throw new Error("Run history identity mismatch");
                }
                // Old history remains bounded to the metadata page. Process its
                // cursor identities without materializing off-screen old runs.
                if (event.run_id < session.historyFloor) continue;
                applyDurable(session, event, protectedTerminals);
              }
              if (
                page.next_cursor < after ||
                (page.has_more && page.next_cursor <= after)
              ) {
                throw new Error("Run history cursor did not advance");
              }
              // Only a fully processed page can advance the cursor. A page's
              // high-water may include records still on later pages.
              session.cursor = page.next_cursor;
              more = page.has_more;
            }
          }
        } catch (cause) {
          if (current(session))
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
          session.drain = null;
        }
      };
      const applyDurable = (
        owner: ConversationSession,
        event: DurableRunEvent,
        protectedTerminals: Set<number>
      ) => {
        patch(owner, event.run_id, (run) => {
          if (event.step) {
            // Only wholly unloaded terminal history stays lazy. A partial
            // live list must accept missing committed steps even when Finished
            // arrives before this page; its nonempty list will not trigger the
            // component's lazy history load.
            if (
              !run.steps_loaded &&
              run.steps.length === 0 &&
              !isActive(run.status) &&
              event.event_id <= (owner.initialHighWater ?? 0)
            )
              return run;
            return {
              ...run,
              steps: mergeSteps(run.steps, [toStepView(event.step)]),
            };
          }
          // A read issued before a newer terminal live frame or metadata
          // snapshot cannot replace that terminal presentation on completion.
          if (event.terminal && protectedTerminals.has(event.run_id))
            return run;
          if (event.terminal)
            return {
              ...run,
              status: event.terminal.status,
              error: event.terminal.error,
              finished_at: run.finished_at ?? event.created_at,
              pending_approval: null,
              pending_question: null,
            };
          return run;
        });
      };
      session.drain = drain();
      return session.drain;
    },
    [current, patch]
  );

  const reload = useCallback(async (): Promise<void> => {
    const session = sessionRef.current;
    if (!session || session.conversationId !== conversationId) return;
    const request = ++session.request;
    const revisions = new Map(session.revision);
    setLoading(true);
    setError(null);
    try {
      const fetched = await listAgentRuns(session.conversationId, {
        limit: REHYDRATION_RUN_PAGE,
      });
      if (!current(session) || request !== session.request) return;
      const owned = fetched.filter(
        (run) => run.conversation_id === session.conversationId
      );
      if (!session.ready)
        session.historyFloor = owned.length
          ? Math.min(...owned.map((run) => run.id))
          : 0;
      const hasRunning = owned.some((run) => run.status === "running");
      const live = hasRunning
        ? await inspectRunLive(session.conversationId).catch(() => undefined)
        : undefined;
      const persistedPendingQuestions =
        hasRunning &&
        live?.parked_reason === "awaiting_question" &&
        !live?.pending_question
          ? await listRunQuestions(session.conversationId).catch(() => [])
          : [];
      const needsPlanFetch = owned.some(
        (run) => run.is_plan && !run.plan_content
      );
      const persistedPlans = needsPlanFetch
        ? await listRunPlans(session.conversationId).catch(() => [])
        : [];
      const plansByRun = new Map(persistedPlans.map((p) => [p.run_id, p]));
      if (!current(session) || request !== session.request) return;
      const views = owned.map((row): AgentRunView => {
        const old = session.runs.find((run) => run.run_id === row.id);
        const status = row.interrupted ? "interrupted" : row.status;
        const planRow = plansByRun.get(row.id);
        const changed =
          (session.revision.get(row.id) ?? 0) !== (revisions.get(row.id) ?? 0);
        const preserve =
          old && (changed || (!isActive(old.status) && isActive(status)));
        const merged: AgentRunView = {
          ...placeholder(row.id, session.conversationId),
          ...old,
          steps_loaded:
            old?.steps_loaded === true && row.total_steps <= old.steps.length,
          model: row.model,
          started_at: row.started_at,
          status: preserve ? old.status : status,
          interrupted: Boolean(row.interrupted),
          finished_at: preserve ? old.finished_at : row.finished_at,
          error: preserve ? old.error : row.error,
          final_content: preserve
            ? old.final_content
            : (row.final_content ?? old?.final_content ?? null),
          is_plan: row.is_plan ?? old?.is_plan ?? false,
          plan_content:
            row.plan_content ??
            planRow?.plan_content ??
            old?.plan_content ??
            (row.is_plan ? (row.final_content ?? old?.final_content ?? null) : null),
          plan_decision_status:
            row.plan_decision_status ??
            planRow?.decision_status ??
            old?.plan_decision_status ??
            null,
          plan_decision_reply:
            row.plan_decision_reply ??
            planRow?.decision_reply ??
            old?.plan_decision_reply ??
            null,
          plan_implemented_run_id:
            row.plan_implemented_run_id ??
            planRow?.implemented_run_id ??
            old?.plan_implemented_run_id ??
            null,
          continued_from_run_id:
            row.continued_from_run_id ?? old?.continued_from_run_id ?? null,
          total_steps: Math.max(
            row.total_steps,
            old?.total_steps ?? 0,
            old?.steps.length ?? 0
          ),
        };
        // Only the live server snapshot can restore a parked consent UI.
        // Persisted history never creates executable or pending approvals.
        merged.pending_approval = isActive(merged.status)
          ? !changed && live !== undefined
            ? live?.run_id === row.id
              ? live.pending_approval
              : null
            : (old?.pending_approval ?? null)
          : null;
        if (isActive(merged.status)) {
          if (!changed && live !== undefined) {
            if (live?.run_id === row.id && live.pending_question) {
              merged.pending_question = live.pending_question;
            } else {
              const fallback = persistedPendingQuestions.find(
                (q) => q.run_id === row.id && q.status === "pending"
              );
              merged.pending_question = fallback
                ? {
                    question_id: fallback.id,
                    run_id: fallback.run_id,
                    conversation_id: fallback.conversation_id,
                    call_id: fallback.call_id,
                    question: fallback.question,
                    options: fallback.options,
                    multi_select: fallback.multi_select,
                    allow_free_text: fallback.allow_free_text,
                    created_at: fallback.created_at,
                  }
                : null;
            }
          } else {
            merged.pending_question = old?.pending_question ?? null;
          }
        } else {
          merged.pending_question = null;
        }
        if (
          old &&
          (merged.status !== old.status ||
            merged.error !== old.error ||
            merged.finished_at !== old.finished_at)
        ) {
          session.revision.set(row.id, (session.revision.get(row.id) ?? 0) + 1);
        }
        return merged;
      });
      const ids = new Set(views.map((run) => run.run_id));
      publish(session, [
        ...views,
        ...session.runs.filter((run) => !ids.has(run.run_id)),
      ]);
      session.ready = true;
      await catchUp(session);
    } catch (cause) {
      if (current(session) && request === session.request) {
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    } finally {
      if (current(session) && request === session.request) setLoading(false);
    }
  }, [conversationId, current, publish, catchUp]);

  const loadRunSteps = useCallback(
    async (runId: number): Promise<void> => {
      const session = sessionRef.current;
      if (
        !session ||
        session.stepsLoading.has(runId) ||
        session.runs.find((run) => run.run_id === runId)?.steps_loaded
      )
        return;
      session.stepsLoading.add(runId);
      try {
        const raw = await listAgentSteps(runId);
        if (!current(session)) return;
        if (raw.some((step) => step.run_id !== runId)) return;
        publish(
          session,
          session.runs.map((run) =>
            run.run_id !== runId
              ? run
              : {
                  ...run,
                  steps: mergeSteps(run.steps, raw.map(toStepView)),
                  steps_loaded: true,
                }
          )
        );
      } catch {
        // Best effort; visibility or a user expand can retry.
      } finally {
        // This set belongs to the originating generation, including on failure.
        session.stepsLoading.delete(runId);
      }
    },
    [current, publish]
  );

  useEffect(() => {
    const session = sessionRef.current;
    if (!session || session.runs !== runs) return;
    // Late frames can partially materialize terminal history after its journal
    // records were acknowledged while unloaded. A nonempty accordion does not
    // request lazy history, so repair that incomplete list with the same fenced,
    // deduplicated reader. Untouched terminal lists remain lazy; a later reload
    // publishes a fresh snapshot and can retry a failed read.
    for (const run of runs) {
      if (!isActive(run.status) && !run.steps_loaded && run.steps.length > 0) {
        void loadRunSteps(run.run_id);
      }
    }
  }, [runs, loadRunSteps]);

  const collapseStep = useCallback(
    (runId: number, seq: number) => {
      const session = sessionRef.current;
      if (!session) return;
      session.observations.delete(`${runId}:${seq}`);
      publish(
        session,
        session.runs.map((run) =>
          run.run_id !== runId
            ? run
            : {
                ...run,
                steps: run.steps.map((step) =>
                  step.seq !== seq
                    ? step
                    : {
                        ...step,
                        observation_full: null,
                        observation_error: null,
                        observation_loading: false,
                      }
                ),
              }
        )
      );
    },
    [publish]
  );

  const loadRunCheckpoints = useCallback(
    async (runId: number): Promise<void> => {
      const session = sessionRef.current;
      if (
        !session ||
        session.checkpointsLoading.has(runId) ||
        !session.runs.some((run) => run.run_id === runId)
      )
        return;
      session.checkpointsLoading.add(runId);
      try {
        const list = await listRunCheckpoints(runId);
        if (!current(session)) return;
        patch(session, runId, (run) => ({
          ...run,
          checkpoints: list,
          checkpoints_loaded: true,
        }));
      } catch {
        // Best effort; expanding again or a manual action can retry.
      } finally {
        session.checkpointsLoading.delete(runId);
      }
    },
    [current, patch]
  );

  const revertCheckpoint = useCallback(
    async (
      runId: number,
      checkpointId: number,
      force = false
    ): Promise<RevertOutcome> => {
      const session = sessionRef.current;
      try {
        const updated = await revertFileCheckpoint(checkpointId, force);
        if (session && current(session)) {
          patch(session, runId, (run) => {
            const existing = run.checkpoints ?? [];
            const next = existing.some((cp) => cp.id === updated.id)
              ? existing.map((cp) => (cp.id === updated.id ? updated : cp))
              : [...existing, updated];
            return { ...run, checkpoints: next };
          });
        }
        return { status: "reverted" };
      } catch (cause) {
        return parseRevertError(cause);
      }
    },
    [current, patch]
  );

  const revertTurn = useCallback(
    async (
      runId: number,
      turnIndex: number,
      force = false
    ): Promise<RevertOutcome> => {
      const session = sessionRef.current;
      try {
        const updatedList = await revertTurnCheckpoints(runId, turnIndex, force);
        if (session && current(session)) {
          patch(session, runId, (run) => ({
            ...run,
            checkpoints: updatedList,
            checkpoints_loaded: true,
          }));
        }
        return { status: "reverted" };
      } catch (cause) {
        return parseRevertError(cause);
      }
    },
    [current, patch]
  );

  const expandStep = useCallback(
    async (runId: number, seq: number): Promise<void> => {
      const session = sessionRef.current;
      if (!session) return;
      const step = session.runs
        .find((run) => run.run_id === runId)
        ?.steps.find((item) => item.seq === seq);
      if (
        !step ||
        !step.step_id ||
        step.observation_full !== null ||
        step.observation_loading
      )
        return;
      const key = `${runId}:${seq}`;
      const token = Symbol();
      session.observations.set(key, token);
      const write = (fields: Partial<AgentStepView>) => {
        if (!current(session) || session.observations.get(key) !== token)
          return;
        publish(
          session,
          session.runs.map((run) =>
            run.run_id !== runId
              ? run
              : {
                  ...run,
                  steps: run.steps.map((item) =>
                    item.step_id !== step.step_id
                      ? item
                      : { ...item, ...fields }
                  ),
                }
          )
        );
      };
      write({ observation_loading: true, observation_error: null });
      try {
        const full = await getAgentStepObservation(step.step_id);
        write({
          observation_full: full,
          observation_loading: false,
          observation_error: null,
        });
      } catch (cause) {
        write({
          observation_loading: false,
          observation_error:
            cause instanceof Error ? cause.message : String(cause),
        });
      } finally {
        if (session.observations.get(key) === token)
          session.observations.delete(key);
      }
    },
    [current, publish]
  );

  useEffect(() => {
    const session: ConversationSession | null =
      conversationId === null
        ? null
        : {
            conversationId,
            runs: [],
            request: 0,
            revision: new Map(),
            stepsLoading: new Set(),
            checkpointsLoading: new Set(),
            observations: new Map(),
            cursor: 0,
            initialHighWater: null,
            historyFloor: 0,
            ready: false,
            dirty: false,
            drain: null,
          };
    sessionRef.current = session;
    setRuns([]);
    setError(null);
    setLoading(false);
    if (!session) return;
    let unlisten: (() => void) | undefined;
    void reload();
    void listen<AgentRunEventPayload>("agent-run-event", ({ payload }) => {
      if (!current(session)) return;
      const owner =
        payload.type === "finished"
          ? payload.event.conversation_id
          : payload.conversation_id;
      if (owner !== session.conversationId) return;
      if (payload.type === "durable") {
        void catchUp(session);
      } else if (payload.type === "step") {
        const mutatingDone =
          payload.event.status === "succeeded" &&
          (payload.event.tool_name === "write_file" ||
            payload.event.tool_name === "edit_file");
        patch(session, payload.run_id, (run) => ({
          ...run,
          steps: mergeSteps(run.steps, [
            toStepViewFromFrame(payload.run_id, payload.event),
          ]),
          streaming_text: undefined,
          streaming_thinking: undefined,
          steps_loaded:
            run.steps_loaded ||
            !session.runs.some((item) => item.run_id === payload.run_id),
          checkpoints_loaded: mutatingDone ? false : run.checkpoints_loaded,
        }));
      } else if (payload.type === "finished") {
        const run = session.runs.find((item) => item.run_id === payload.run_id);
        patch(session, payload.run_id, (old) => ({
          ...old,
          status: payload.event.status,
          error: payload.event.error,
          final_content:
            payload.event.final_content ?? old.final_content ?? null,
          plan_content:
            old.plan_content ??
            (old.is_plan && payload.event.status === "completed"
              ? (payload.event.final_content ?? null)
              : null),
          plan_decision_status:
            old.plan_decision_status ??
            (old.is_plan && payload.event.status === "completed"
              ? "pending"
              : null),
          finished_at: old.finished_at ?? Date.now() / 1000,
          pending_approval: null,
          pending_question: null,
          streaming_text: undefined,
          streaming_thinking: undefined,
          checkpoints_loaded: false,
        }));
        if (!run || run.steps.length === 0) void loadRunSteps(payload.run_id);
      } else {
        const gov = payload.event;
        patch(session, payload.run_id, (run) => {
          if (gov.type === "plan_ready") {
            return {
              ...run,
              is_plan: true,
              plan_content: gov.plan.plan_content,
              plan_decision_status: gov.plan.decision_status,
              plan_decision_reply: gov.plan.decision_reply,
              plan_implemented_run_id: gov.plan.implemented_run_id,
            };
          }
          if (!isActive(run.status)) return run;
          if (gov.type === "stream_delta") {
            if (gov.kind === "text") {
              return {
                ...run,
                streaming_text: (run.streaming_text ?? "") + gov.delta,
              };
            }
            if (gov.kind === "thinking") {
              const turnSeparator =
                run.streaming_thinking === undefined && run.thinking
                  ? "\n\n"
                  : "";
              return {
                ...run,
                streaming_thinking: (run.streaming_thinking ?? "") + gov.delta,
                thinking: (run.thinking ?? "") + turnSeparator + gov.delta,
              };
            }
            return run;
          }
          if (gov.type === "approval_requested")
            return {
              ...run,
              pending_approval: {
                metadata: gov.metadata,
                call_id: gov.call_id,
                name: gov.name,
                arguments: gov.arguments,
                timeout_secs: gov.timeout_secs ?? 0,
                requested_at: Date.now() / 1000,
              },
            };
          if (gov.type === "approval_resolved")
            return {
              ...run,
              pending_approval:
                run.pending_approval?.call_id === gov.call_id
                  ? null
                  : run.pending_approval,
            };
          if (gov.type === "question_requested")
            return {
              ...run,
              pending_question: gov.question,
            };
          if (gov.type === "question_resolved")
            return {
              ...run,
              pending_question:
                run.pending_question?.question_id === gov.question_id
                  ? null
                  : run.pending_question,
            };
          if (gov.type === "steering_injected")
            return {
              ...run,
              last_steering: gov.content,
            };
          if (gov.type === "cancelled")
            return {
              ...run,
              status: "cancelled",
              pending_approval: null,
              pending_question: null,
            };
          if (
            gov.type === "paused" ||
            gov.type === "budget_exhausted" ||
            gov.type === "spend_limit_exceeded"
          )
            return { ...run, status: gov.type };
          if (gov.type === "resumed") return { ...run, status: "running" };
          return run;
        });
      }
    })
      .then((dispose) => {
        if (!current(session)) {
          dispose();
          return;
        }
        unlisten = dispose;
        // Reconcile the subscription setup gap even if no notification arrives.
        void catchUp(session);
      })
      .catch(() => {
        /* Manual reload still reconciles when listening fails. */
      });
    return () => {
      if (sessionRef.current === session) sessionRef.current = null;
      unlisten?.();
    };
  }, [conversationId, reload, current, patch, catchUp, loadRunSteps]);

  const answerQuestion = useCallback(
    async (
      runId: number,
      questionId: number,
      selectedOptions: string[],
      freeText?: string
    ): Promise<RunQuestion> => {
      const answered = await answerRunQuestion(
        runId,
        questionId,
        selectedOptions,
        freeText
      );
      const session = sessionRef.current;
      if (session && session.conversationId === conversationId) {
        patch(session, runId, (run) => ({
          ...run,
          pending_question:
            run.pending_question?.question_id === questionId
              ? null
              : run.pending_question,
        }));
      }
      return answered;
    },
    [conversationId, patch]
  );

  const clarifyRun = useCallback(
    async (runId: number, content: string): Promise<void> => {
      await clarifyAgentRun(runId, content);
    },
    []
  );

  const applyPlanDecision = useCallback(
    (runId: number, plan: RunPlan): void => {
      const session = sessionRef.current;
      if (!session || session.conversationId !== conversationId) return;
      patch(session, runId, (run) => ({
        ...run,
        is_plan: true,
        plan_content: plan.plan_content,
        plan_decision_status: plan.decision_status,
        plan_decision_reply: plan.decision_reply,
        plan_implemented_run_id: plan.implemented_run_id,
      }));
    },
    [conversationId, patch]
  );

  return {
    runs,
    loading,
    error,
    reload,
    loadRunSteps,
    expandStep,
    collapseStep,
    loadRunCheckpoints,
    revertCheckpoint,
    revertTurn,
    answerQuestion,
    clarifyRun,
    applyPlanDecision,
  };
}
