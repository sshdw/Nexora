//! Agent run live stream + rehydration hook (Task 5.1).
//!
//! Mirrors `useConversation` structure: one `listen` on the static event name
//! `"agent-run-event"`, `run_id` filter, stale-conversation guard,
//! `unlisten` cleanup, and rehydration via `list_agent_runs` on conversation
//! switch. Keeps rehydration as the source of truth.
//!
//! AC-102/ADD-08 (run-rehydration N+1): the initial load fetches a BOUNDED
//! page of runs (`REHYDRATION_RUN_PAGE`) and NO per-run steps fan-out — steps
//! lazy-load through `loadRunSteps` only for a run whose step list is
//! actually shown (AgentRunSteps asks when a run renders expanded with no
//! steps loaded). A terminal `finished` frame PATCHes the affected run from
//! its payload instead of re-firing the full `reload()` fan-out; a run with
//! no steps on screen fetches its now-final step list once.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  type AgentRun,
  type AgentRunEventPayload,
  type AgentStep,
  type GovernanceEventPayload,
  type StepEventFrame,
  getAgentStepObservation,
  listAgentRuns,
  listAgentSteps,
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
  pending_approval: { call_id: string; name: string; arguments: string; timeout_secs: number; requested_at: number } | null;
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

function toStepViewFromFrame(runId: number, frame: StepEventFrame): AgentStepView {
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
}

export function useAgentRun(conversationId: number | null): AgentRunStore {
  const [runs, setRuns] = useState<AgentRunView[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  const activeConversationRef = useRef<number | null>(null);
  // Known run_ids for the active conversation, for live filtering.
  const knownRunIdsRef = useRef<Set<number>>(new Set());
  // Runs with a lazy step fetch in flight (AC-102/ADD-08): a sync set so a
  // StrictMode double-effect or a fast expand/collapse cycle cannot double-
  // fire the same listAgentSteps.
  const stepsLoadingRef = useRef<Set<number>>(new Set());
  // Latest rendered runs, for snapshot reads BEFORE state updaters (expand
  // needs the step's fetch key without assigning inside an updater — the
  // updater must stay pure so a StrictMode double-invoke is harmless).
  const runsRef = useRef<AgentRunView[]>([]);
  useEffect(() => {
    runsRef.current = runs;
  }, [runs]);

  const reload = useCallback(async (): Promise<void> => {
    if (conversationId === null) {
      setRuns([]);
      knownRunIdsRef.current = new Set();
      return;
    }
    const requestedId = conversationId;
    setLoading(true);
    setError(null);
    try {
      // AC-102/ADD-08: one bounded page of runs — the newest
      // REHYDRATION_RUN_PAGE — and NO per-run steps fan-out. Step lists
      // lazy-load through `loadRunSteps` only for runs whose step list is
      // actually shown (AgentRunSteps asks; the store dedupes).
      const fetched: AgentRun[] = await listAgentRuns(requestedId, {
        limit: REHYDRATION_RUN_PAGE,
      });
      // Only the active conversation may update state.
      if (activeConversationRef.current !== requestedId) return;
      const views: AgentRunView[] = fetched.map((run) => ({
        run_id: run.id,
        conversation_id: run.conversation_id ?? requestedId,
        status: run.status,
        model: run.model,
        started_at: run.started_at,
        finished_at: run.finished_at,
        error: run.error,
        steps: [],
        steps_loaded: false,
        total_steps: run.total_steps,
        pending_approval: null,
      }));
      if (activeConversationRef.current !== requestedId) return;
      // Preserve live pending_approvals across reloads? Reload is source of truth,
      // so pending approvals come only from live stream, not persistence.
      // Merge with previous pending state for still-running runs if possible.
      setRuns((prev) => {
        if (activeConversationRef.current !== requestedId) return prev;
        // Keep pending_approval for running runs if we had it.
        const prevById = new Map(prev.map((p) => [p.run_id, p.pending_approval]));
        return views.map((v) => ({
          ...v,
          pending_approval: v.status === "running" ? (prevById.get(v.run_id) ?? null) : null,
        }));
      });
      knownRunIdsRef.current = new Set(views.map((v) => v.run_id));
    } catch (e) {
      if (activeConversationRef.current !== requestedId) return;
      setRuns([]);
      knownRunIdsRef.current = new Set();
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (activeConversationRef.current === requestedId) setLoading(false);
    }
  }, [conversationId]);

  /** AC-102/ADD-08 lazy step load: one `list_agent_steps` for a run whose
   *  persisted step list is (about to be) shown. A no-op for live runs —
   *  their steps streamed in — and for runs already loaded or in flight.
   *  Unknown run ids proceed (a just-finished placeholder is about to exist
   *  by the time the fetch resolves) and patch in only if the run renders. */
  const loadRunSteps = useCallback(async (runId: number): Promise<void> => {
    const requestedConv = activeConversationRef.current;
    if (requestedConv === null) return;
    if (stepsLoadingRef.current.has(runId)) return;
    const known = runsRef.current.find((r) => r.run_id === runId);
    if (known !== undefined && known.steps_loaded) return;
    stepsLoadingRef.current.add(runId);
    try {
      const raw = await listAgentSteps(runId);
      if (activeConversationRef.current !== requestedConv) return;
      // list_steps is ORDER BY seq ASC, gap-free per CF-01.
      const steps = raw.map(toStepView);
      setRuns((prev) =>
        prev.map((run) =>
          run.run_id !== runId || run.steps_loaded
            ? run
            : { ...run, steps, steps_loaded: true },
        ),
      );
    } catch {
      // Best-effort: the run pill still renders; the next visibility change
      // asks again.
    } finally {
      stepsLoadingRef.current.delete(runId);
    }
  }, []);

  const collapseStep = useCallback((runId: number, seq: number): void => {
    setRuns((prev) =>
      prev.map((run) =>
        run.run_id !== runId
          ? run
          : {
              ...run,
              steps: run.steps.map((step) =>
                step.seq !== seq
                  ? step
                  // Also clear a stale in-flight flag: an expand resolve that
                  // lands after this collapse must not resurrect the dropped
                  // full text (its write is guarded on this flag).
                  : { ...step, observation_full: null, observation_error: null, observation_loading: false },
              ),
            },
      ),
    );
  }, []);

  const expandStep = useCallback(async (runId: number, seq: number): Promise<void> => {
    const requestedConv = activeConversationRef.current;
    // Fetch key resolved from the latest snapshot BEFORE any updater runs:
    // no assignment inside the updater below, so it stays a pure function
    // of `prev` and a StrictMode double-invoke returns the same state.
    const snap = runsRef.current.find((r) => r.run_id === runId)?.steps.find((s) => s.seq === seq);
    const stepId = snap?.step_id ?? null;
    if (stepId === null || stepId === 0) {
      setRuns((prev) =>
        prev.map((run) =>
          run.run_id !== runId
            ? run
            : {
                ...run,
                steps: run.steps.map((step) =>
                  step.seq !== seq
                    ? step
                    : { ...step, observation_loading: false },
                ),
              },
        ),
      );
      return;
    }
    if (snap === undefined || snap.observation_full !== null || snap.observation_loading) return;
    // Optimistic loading flag, set after the key resolve. The per-item
    // condition keeps the updater pure: steps that already hold the full
    // text (or are already loading) pass through untouched.
    setRuns((prev) =>
      prev.map((r) =>
        r.run_id !== runId
          ? r
          : {
              ...r,
              steps: r.steps.map((s) =>
                s.seq !== seq || s.observation_full !== null || s.observation_loading
                  ? s
                  : { ...s, observation_loading: true, observation_error: null },
              ),
            },
      ),
    );
    try {
      const full = await getAgentStepObservation(stepId);
      if (activeConversationRef.current !== requestedConv) return;
      // Guarded write: only a step still showing loading takes the full
      // text. A collapse in flight clears the flag (and drops the text), so
      // a late resolve must not resurrect it. Overlapping expands cannot
      // stack — a second expand returns early on the loading flag above —
      // so no generation counter is needed.
      setRuns((prev) =>
        prev.map((run) =>
          run.run_id !== runId
            ? run
            : {
                ...run,
                steps: run.steps.map((step) =>
                  step.seq !== seq || !step.observation_loading
                    ? step
                    : {
                        ...step,
                        observation_full: full,
                        observation_loading: false,
                        observation_error: null,
                      },
                ),
              },
        ),
      );
    } catch (e) {
      if (activeConversationRef.current !== requestedConv) return;
      setRuns((prev) =>
        prev.map((run) =>
          run.run_id !== runId
            ? run
            : {
                ...run,
                steps: run.steps.map((step) =>
                  step.seq !== seq
                    ? step
                    : {
                        ...step,
                        observation_loading: false,
                        observation_error: e instanceof Error ? e.message : String(e),
                      },
                ),
              },
        ),
      );
    }
  }, []);

  useEffect(() => {
    activeConversationRef.current = conversationId;
    setRuns([]);
    knownRunIdsRef.current = new Set();
    stepsLoadingRef.current = new Set();
    setError(null);
    if (conversationId !== null) void reload();
  }, [conversationId, reload]);

  // Single listen on the static event name; filter by run_id.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;

    const setup = async () => {
      try {
        unlisten = await listen<AgentRunEventPayload>("agent-run-event", (event) => {
          const payload = event.payload;
          const activeId = activeConversationRef.current;
          if (activeId === null) return;
          // Stale payload? Ignore if its run_id not known yet and not for active conv.
          // For finished frames we can check conversation_id directly.
          if (payload.type === "finished") {
            if (payload.event.conversation_id !== activeId) return;
            // AC-102/ADD-08 patch-on-finish: the terminal frame carries the
            // run's outcome, so PATCH the affected run instead of re-firing
            // the full rehydration fan-out (which re-fetched every run's
            // steps on every completion). A run with no steps on screen —
            // rehydrated unloaded, or an unknown run — fetches its now-final
            // step list once, via the same lazy path as expansion.
            const finished = runsRef.current.find((r) => r.run_id === payload.run_id);
            setRuns((prev) => {
              const idx = prev.findIndex((r) => r.run_id === payload.run_id);
              if (idx >= 0) {
                const copy = [...prev];
                copy[idx] = { ...copy[idx], status: payload.event.status, finished_at: Date.now() / 1000, error: payload.event.error };
                return copy;
              }
              // Unknown run that finished for this conversation: add a
              // placeholder; the lazy fetch below fills its final steps.
              return [
                ...prev,
                {
                  run_id: payload.run_id,
                  conversation_id: payload.event.conversation_id,
                  status: payload.event.status,
                  model: "",
                  started_at: Date.now() / 1000,
                  finished_at: Date.now() / 1000,
                  error: payload.event.error,
                  steps: [],
                  steps_loaded: false,
                  total_steps: null,
                  pending_approval: null,
                },
              ];
            });
            knownRunIdsRef.current.add(payload.run_id);
            if (finished === undefined || finished.steps.length === 0) {
              void loadRunSteps(payload.run_id);
            }
            return;
          }

          // For step/governance, filter by known run_ids. If unknown run_id,
          // it might be a new run started in this conversation that hasn't been
          // rehydrated yet. Allow it if we have no known runs and the payload
          // is for this conversation? We can't know conversation from step.
          // Pragmatic: allow unknown run_ids to create a placeholder run; if it
          // belonged to another conversation, its finished frame will have
          // mismatched conversation_id and we will have filtered it above, but
          // its steps would have been incorrectly added. To avoid that, only
          // accept unknown run_ids if they were just started; we optimistically
          // accept and let finished filter correct. The leak window is small.
          // Better: ignore unknown until finished triggers reload, but then steps
          // won't show live. So we create placeholder.
          const runId = payload.run_id;

          if (payload.type === "step") {
            const frame = payload.event;
            setRuns((prev) => {
              const idx = prev.findIndex((r) => r.run_id === runId);
              if (idx >= 0) {
                const run = prev[idx];
                // Duplicate seq is idempotent — ignore if already present.
                if (run.steps.some((s) => s.seq === frame.seq)) return prev;
                const newStep = toStepViewFromFrame(runId, frame);
                const updated = [...prev];
                const newSteps = [...run.steps, newStep].sort((a, b) => a.seq - b.seq);
                updated[idx] = { ...run, steps: newSteps };
                return updated;
              }
              // Unknown run: create placeholder running run so steps aren't lost.
              // We don't know conversation_id here; assume activeId and correct
              // later via finished. If it's actually for another conversation,
              // the steps will be orphaned until reload replaces state; the
              // finished filter will have already ensured we only keep runs
              // for active conv, so orphan will be cleaned on next reload.
              // To avoid cross-conv bleed, we could check knownRunIds size?
              // For now, allow placeholder only if we consider it live.
              knownRunIdsRef.current.add(runId);
              return [
                ...prev,
                {
                  run_id: runId,
                  conversation_id: activeId,
                  status: "running",
                  model: "",
                  started_at: Date.now() / 1000,
                  finished_at: null,
                  error: null,
                  steps: [toStepViewFromFrame(runId, frame)],
                  // Live run: its steps stream in, nothing to lazy-load.
                  steps_loaded: true,
                  total_steps: null,
                  pending_approval: null,
                },
              ];
            });
            // Ensure known set includes this run even if we didn't add (race)
            knownRunIdsRef.current.add(runId);
          } else if (payload.type === "governance") {
            const gov: GovernanceEventPayload = payload.event;
            if (gov.type === "approval_requested") {
              setRuns((prev) => {
                const idx = prev.findIndex((r) => r.run_id === runId);
                if (idx >= 0) {
                  const copy = [...prev];
                  copy[idx] = {
                    ...copy[idx],
                    pending_approval: {
                      call_id: gov.call_id,
                      name: gov.name,
                      arguments: gov.arguments,
                      timeout_secs: gov.timeout_secs ?? 0,
                      requested_at: Date.now() / 1000,
                    },
                  };
                  return copy;
                }
                // Unknown run approval: create placeholder
                knownRunIdsRef.current.add(runId);
                return [
                  ...prev,
                  {
                    run_id: runId,
                    conversation_id: activeId,
                    status: "running",
                    model: "",
                    started_at: Date.now() / 1000,
                    finished_at: null,
                    error: null,
                    steps: [],
                    // Live run: steps stream in, nothing to lazy-load.
                    steps_loaded: true,
                    total_steps: null,
                    pending_approval: {
                      call_id: gov.call_id,
                      name: gov.name,
                      arguments: gov.arguments,
                      timeout_secs: gov.timeout_secs ?? 0,
                      requested_at: Date.now() / 1000,
                    },
                  },
                ];
              });
            } else if (gov.type === "approval_resolved") {
              setRuns((prev) => {
                const idx = prev.findIndex((r) => r.run_id === runId);
                if (idx < 0) return prev;
                const copy = [...prev];
                // Clear pending only if it matches the resolved call_id
                const cur = copy[idx].pending_approval;
                if (cur && cur.call_id === gov.call_id) copy[idx] = { ...copy[idx], pending_approval: null };
                return copy;
              });
            } else if (gov.type === "budget_exhausted") {
              setRuns((prev) => {
                const idx = prev.findIndex((r) => r.run_id === runId);
                if (idx < 0) return prev;
                const copy = [...prev];
                copy[idx] = { ...copy[idx], status: "budget_exhausted" };
                return copy;
              });
            } else if (gov.type === "spend_limit_exceeded") {
              setRuns((prev) => {
                const idx = prev.findIndex((r) => r.run_id === runId);
                if (idx < 0) return prev;
                const copy = [...prev];
                copy[idx] = { ...copy[idx], status: "spend_limit_exceeded" };
                return copy;
              });
            } else if (gov.type === "cancelled") {
              // The server emits a plain `cancelled` with no reason (a user
              // cancel and an expired approval park look identical here), so
              // the pending approval must clear: its buttons must not stay
              // live past the run's end. Any "expired" wording from here on
              // is the server-confirmed state, never client math.
              setRuns((prev) => {
                const idx = prev.findIndex((r) => r.run_id === runId);
                if (idx < 0) return prev;
                const copy = [...prev];
                copy[idx] = { ...copy[idx], status: "cancelled", pending_approval: null };
                return copy;
              });
            } else if (gov.type === "paused") {
              setRuns((prev) => {
                const idx = prev.findIndex((r) => r.run_id === runId);
                if (idx < 0) return prev;
                const copy = [...prev];
                copy[idx] = { ...copy[idx], status: "paused" };
                return copy;
              });
            } else if (gov.type === "resumed") {
              setRuns((prev) => {
                const idx = prev.findIndex((r) => r.run_id === runId);
                if (idx < 0) return prev;
                const copy = [...prev];
                copy[idx] = { ...copy[idx], status: "running" };
                return copy;
              });
            } else if (gov.type === "completed") {
              // Completed governance event is informational; final status comes from finished frame.
              // Keep as running until finished reload.
            }
          }
        });
        if (cancelled && unlisten) {
          unlisten();
          unlisten = null;
        }
      } catch {
        // Listen failed; hook remains in rehydration-only mode.
      }
      return () => {
        if (unlisten) unlisten();
      };
    };

    const cleanupPromise = setup();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
      else void cleanupPromise.then((fn) => { if (fn) fn(); });
    };
  }, [reload, loadRunSteps]);

  return { runs, loading, error, reload, loadRunSteps, expandStep, collapseStep };
}
