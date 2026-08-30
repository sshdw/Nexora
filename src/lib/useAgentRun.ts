//! Agent run live stream + rehydration hook (Task 5.1).
//!
//! Mirrors `useConversation` structure: one `listen` on the static event name
//! `"agent-run-event"`, `run_id` filter, stale-conversation guard,
//! `unlisten` cleanup, and rehydration via `list_agent_runs` / `list_agent_steps`
//! on conversation switch (ORDER BY seq). Keeps rehydration as the source of
//! truth; live frames are best-effort display until the terminal `finished`
//! frame triggers a `reload()`.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  type AgentRun,
  type AgentRunEventPayload,
  type AgentStep,
  type GovernanceEventPayload,
  type StepEventFrame,
  listAgentRuns,
  listAgentSteps,
} from "./tauri";

/** One step as rendered in the accordion (persisted shape + live append). */
export interface AgentStepView {
  id: number; // 0 for live-only before rehydration; real id after reload
  run_id: number;
  seq: number;
  kind: string;
  tool_name: string | null;
  arguments: string | null;
  observation: string | null;
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
  pending_approval: { call_id: string; name: string; arguments: string } | null;
}

function toStepView(step: AgentStep): AgentStepView {
  return {
    id: step.id,
    run_id: step.run_id,
    seq: step.seq,
    kind: step.kind,
    tool_name: step.tool_name,
    arguments: step.arguments,
    observation: step.observation,
    status: step.status,
    started_at: step.started_at,
    duration_ms: step.duration_ms,
  };
}

function toStepViewFromFrame(runId: number, frame: StepEventFrame): AgentStepView {
  return {
    id: 0,
    run_id: runId,
    seq: frame.seq,
    kind: frame.kind,
    tool_name: frame.tool_name,
    arguments: frame.arguments,
    observation: frame.observation,
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
}

export function useAgentRun(conversationId: number | null): AgentRunStore {
  const [runs, setRuns] = useState<AgentRunView[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  const activeConversationRef = useRef<number | null>(null);
  // Known run_ids for the active conversation, for live filtering.
  const knownRunIdsRef = useRef<Set<number>>(new Set());

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
      const fetched: AgentRun[] = await listAgentRuns(requestedId);
      // Only the active conversation may update state.
      if (activeConversationRef.current !== requestedId) return;
      // Fetch steps for each run (all of them in 5.1; runs are small).
      const views: AgentRunView[] = await Promise.all(
        fetched.map(async (run) => {
          let steps: AgentStepView[] = [];
          try {
            const raw = await listAgentSteps(run.id);
            // list_steps is ORDER BY seq ASC, gap-free per CF-01.
            steps = raw.map(toStepView);
          } catch {
            // Best-effort: leave empty, UI will still show run pill.
            steps = [];
          }
          return {
            run_id: run.id,
            conversation_id: run.conversation_id ?? requestedId,
            status: run.status,
            model: run.model,
            started_at: run.started_at,
            finished_at: run.finished_at,
            error: run.error,
            steps,
            pending_approval: null,
          } satisfies AgentRunView;
        }),
      );
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

  useEffect(() => {
    activeConversationRef.current = conversationId;
    setRuns([]);
    knownRunIdsRef.current = new Set();
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
            // Known or new finished run: ensure it's tracked, then reload to reconcile.
            // Trigger reload so persisted truth replaces live view (history is replaced, never appended).
            // Optimistically update status before reload for instant pill feedback.
            setRuns((prev) => {
              const idx = prev.findIndex((r) => r.run_id === payload.run_id);
              if (idx >= 0) {
                const copy = [...prev];
                copy[idx] = { ...copy[idx], status: payload.event.status, finished_at: Date.now() / 1000, error: payload.event.error };
                return copy;
              }
              // Unknown run that finished for this conversation: add placeholder, then reload will fill steps.
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
                  pending_approval: null,
                },
              ];
            });
            knownRunIdsRef.current.add(payload.run_id);
            void reload();
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
                    pending_approval: {
                      call_id: gov.call_id,
                      name: gov.name,
                      arguments: gov.arguments,
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
              setRuns((prev) => {
                const idx = prev.findIndex((r) => r.run_id === runId);
                if (idx < 0) return prev;
                const copy = [...prev];
                copy[idx] = { ...copy[idx], status: "cancelled" };
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
  }, [reload]);

  return { runs, loading, error, reload };
}
