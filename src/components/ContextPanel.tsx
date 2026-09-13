//! Read-only Context panel: per-conversation token/cost breakdown stats +
//! changed-files diff list (no agent behavior change).
//!
//! Presentational over the existing IPC wrappers: stats come from the new
//! `conversation_context_stats` command; diffs are grouped client-side from
//! the already-persisted `write_file` step observations (`list_agent_runs` /
//! `list_agent_steps`). Diff rendering reuses the existing `DiffView`
//! classifier from `AgentRunSteps` — no new diff engine.

import { useEffect, useState } from "react";

import {
  conversationContextStats,
  listAgentRuns,
  listAgentSteps,
  type AgentStep,
  type ConversationContextStats,
} from "../lib/tauri";
import { DiffView } from "./AgentRunSteps";

export interface ChangedFile {
  path: string;
  observation: string;
  runId: number;
  seq: number;
}

function parseWritePath(args: string | null): string | null {
  if (!args) return null;
  try {
    const parsed = JSON.parse(args) as Record<string, unknown>;
    const path = parsed.path;
    return typeof path === "string" && path.length > 0 ? path : null;
  } catch {
    return null;
  }
}

function formatCount(value: number): string {
  return value.toLocaleString("en-US").replace(/,/g, " ");
}

function formatTokens(value: number, known: boolean): string {
  if (!known) return "n/a";
  return formatCount(value);
}

function formatCost(microUsd: number): string {
  const usd = microUsd / 1_000_000;
  return `${usd.toFixed(2)} $`;
}

function formatDateTime(ts: number): string {
  const d = new Date(ts * 1000);
  return d.toLocaleString("en-GB", {
    day: "2-digit",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export default function ContextPanel({ conversationId }: { conversationId: number }) {
  const [stats, setStats] = useState<ConversationContextStats | null>(null);
  const [files, setFiles] = useState<ChangedFile[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    setStats(null);
    setFiles([]);

    const load = async () => {
      try {
        const data = await conversationContextStats(conversationId);
        if (cancelled) return;
        setStats(data);
        const runs = await listAgentRuns(conversationId);
        if (cancelled) return;
        const byPath = new Map<string, ChangedFile>();
        for (const run of runs) {
          let steps: AgentStep[] = [];
          try {
            steps = await listAgentSteps(run.id);
          } catch {
            steps = [];
          }
          for (const step of steps) {
            if (step.kind !== "tool_call" || step.tool_name !== "write_file") continue;
            if (!step.observation) continue;
            const path = parseWritePath(step.arguments) ?? `run ${run.id} · step ${step.seq}`;
            const prev = byPath.get(path);
            if (!prev || step.seq >= prev.seq) {
              byPath.set(path, {
                path,
                observation: step.observation,
                runId: run.id,
                seq: step.seq,
              });
            }
          }
        }
        if (cancelled) return;
        setFiles([...byPath.values()].sort((a, b) => a.path.localeCompare(b.path)));
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    void load();
    return () => {
      cancelled = true;
    };
  }, [conversationId]);

  if (loading) {
    return (
      <p className="nex-thread-status nex-fade-in" role="status">
        Loading context…
      </p>
    );
  }

  if (error || !stats) {
    return (
      <div className="nex-composer-error nex-fade-in" role="alert">
        {error ?? "Context stats are unavailable."}
      </div>
    );
  }

  const user = stats.user_message_count;
  const assistant = stats.assistant_message_count;
  const tools = stats.tool_call_count;
  const other = stats.other_step_count;
  const total = user + assistant + tools + other;
  const pct = (v: number): number => (total === 0 ? 0 : (v / total) * 100);

  const rows: Array<{ label: string; value: string; title?: string }> = [
    { label: "Session", value: stats.title },
    { label: "Messages", value: formatCount(stats.message_count) },
    {
      label: "Provider",
      value: stats.provider_display ?? stats.provider ?? "n/a",
    },
    { label: "Model", value: stats.model ?? "n/a" },
    { label: "Context limit", value: formatCount(stats.context_limit) },
    { label: "Total tokens", value: formatTokens(stats.total_tokens, stats.has_token_data) },
    {
      label: "Usage",
      value: `${stats.usage_percent.toFixed(0)}%`,
    },
    {
      label: "Input tokens",
      value: formatTokens(stats.input_tokens, stats.has_token_data),
      title: stats.has_token_data ? undefined : "No persisted token usage",
    },
    {
      label: "Output tokens",
      value: formatTokens(stats.output_tokens, stats.has_token_data),
      title: stats.has_token_data ? undefined : "No persisted token usage",
    },
    {
      label: "Reasoning tokens",
      value: formatTokens(stats.reasoning_tokens, stats.has_token_data),
      title: stats.has_token_data ? undefined : "No persisted token usage",
    },
    {
      label: "Cache tokens (read/write)",
      value:
        stats.has_token_data
          ? `${formatCount(stats.cache_read_tokens)} / ${formatCount(stats.cache_write_tokens)}`
          : "n/a",
      title: stats.has_token_data ? undefined : "No persisted token usage",
    },
    { label: "User messages", value: formatCount(stats.user_message_count) },
    { label: "Assistant messages", value: formatCount(stats.assistant_message_count) },
    { label: "Total cost", value: formatCost(stats.total_cost_micro_usd) },
    { label: "Session created", value: formatDateTime(stats.created_at) },
    { label: "Last activity", value: formatDateTime(stats.updated_at) },
  ];

  return (
    <div className="nex-context nex-view-enter" aria-label="Conversation context">
      <dl className="nex-context-grid">
        {rows.map((row) => (
          <div key={row.label} className="nex-context-cell">
            <dt className="nex-context-label">{row.label}</dt>
            <dd className="nex-context-value" title={row.title}>
              {row.value}
            </dd>
          </div>
        ))}
      </dl>

      <section className="nex-context-breakdown" aria-label="Context breakdown">
        <h3 className="nex-context-section-title">Context breakdown</h3>
        <div
          className="nex-context-bar"
          role="img"
          aria-label={`User ${pct(user).toFixed(1)}%, assistant ${pct(assistant).toFixed(1)}%, tool calls ${pct(tools).toFixed(1)}%, other ${pct(other).toFixed(1)}%`}
        >
          <span
            className="nex-context-seg nex-context-seg-user"
            style={{ width: `${pct(user)}%` }}
          />
          <span
            className="nex-context-seg nex-context-seg-assistant"
            style={{ width: `${pct(assistant)}%` }}
          />
          <span
            className="nex-context-seg nex-context-seg-tools"
            style={{ width: `${pct(tools)}%` }}
          />
          <span
            className="nex-context-seg nex-context-seg-other"
            style={{ width: `${pct(other)}%` }}
          />
        </div>
        <ul className="nex-context-legend">
          <li>
            <span className="nex-context-dot nex-context-seg-user" aria-hidden="true" />
            User {pct(user).toFixed(1)}%
          </li>
          <li>
            <span className="nex-context-dot nex-context-seg-assistant" aria-hidden="true" />
            Assistant {pct(assistant).toFixed(1)}%
          </li>
          <li>
            <span className="nex-context-dot nex-context-seg-tools" aria-hidden="true" />
            Tool calls {pct(tools).toFixed(1)}%
          </li>
          <li>
            <span className="nex-context-dot nex-context-seg-other" aria-hidden="true" />
            Other {pct(other).toFixed(1)}%
          </li>
        </ul>
      </section>

      <section className="nex-context-files" aria-label="Changed files">
        <h3 className="nex-context-section-title">
          Changed files: {files.length}
        </h3>
        {files.length === 0 ? (
          <p className="nex-agent-empty">No file changes recorded.</p>
        ) : (
          <ul className="nex-context-file-list">
            {files.map((file) => (
              <li key={`${file.runId}-${file.seq}-${file.path}`} className="nex-context-file">
                <header className="nex-context-file-header">
                  <span className="nex-tag nex-tag-mono" title="Changed file path">
                    {file.path}
                  </span>
                </header>
                <DiffView observation={file.observation} />
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
