# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.2.0] — 2026-09-05

### Added

- Provider wire verification: the four OpenAI-compatible shortlists now list
  10 chat model IDs each, frozen from each provider's live `/models` catalog;
  Settings accepts a custom model ID outside the shortlist.
- HTTP 404 from a compatible endpoint now maps to invalid request
  (model/route missing) instead of the opaque catch-all failure.

## [1.1.0] — 2026-09-05

### Added

- OpenAI-compatible providers xKiro, OpenRouter, NVIDIA NIM, and OpenCode
  Zen behind the single shared `OpenAiExecutor` (distinct endpoint per
  provider, plus OpenRouter Referer/Title headers) with curated hardcoded
  model shortlists; pricing unchanged (5M/25M micro-USD per 1M tokens).

## [1.0.2] — 2026-09-04

### Fixed

- Provider failures now surface their category instead of one generic
  message: rate limit (HTTP 429, with the provider's Retry-After hint),
  outage/overload (HTTP 5xx), network/timeout, rejected credential (401/403),
  invalid request (400), unexpected response — in agent runs and chat alike.
  Error bodies are still never read, so no credential or payload can leak.

## [1.0.1] — 2026-09-04

### Fixed

- **Agent IPC argument naming (post-1.0.0 hotfix)**: all nine agent commands
  (`start_agent_run`, `cancel_agent_run`, `resolve_agent_approval`, `extend_agent_run`,
  `agent_set_mode`, `pause_agent_run`, `resume_agent_run`, `list_agent_runs`,
  `list_agent_steps`) were invoked with snake_case argument keys, while Tauri v2
  deserializes command arguments by camelCase name — so every agent call was rejected
  at IPC validation before reaching the service. The JavaScript layer now sends
  camelCase keys, the browser mock enforces the same contract instead of accepting both
  spellings, and two naming-parity tests keep the frontend and the Rust signatures in sync.
- **Gemini tool-schema rejection (post-1.0.0 hotfix)**: tool schemas are reduced to the
  OpenAPI subset Gemini accepts for all four agent tools (chat and the other providers
  unaffected).
- Fixed: agent runs now return the model's own tool calls and each tool's result to the
  provider in the provider-native format (Gemini functionCall/functionResponse with
  thought-signature round-trip, OpenAI tool_calls/role "tool", Anthropic
  tool_use/tool_result) instead of plain user text, and every run starts with a fixed
  agent system prompt describing the OS, shell and workspace. Before this change the
  model never saw its own tool calls, so multi-step agent tasks could not complete.

## [1.0.0] — 2026-08-30

### Added

- **Agent — three-tier approval gate (Task 4.1)**: the HD-3 autonomy ladder
  (`supervised` / `semi_autonomous` / `full_autonomous`) decides per tool risk
  class whether a workspace tool call executes automatically or parks the run
  until the user approves or denies it; approvals are cancel-safe and
  poison-safe (2026-08-27).
- **Agent — run persistence (Task 4.2)**: `agent_runs` / `agent_steps`
  persistence with an opt-in `RunRecorder`, append-only gap-free step
  sequences, and the CF-01 guarantee that a failed insert emits nothing and
  reuses its sequence (2026-08-28).
- **Providers — Anthropic & Gemini native tool calling (Task 2.2)**: native
  tool-calling parity for both providers plus a configurable
  `request_timeout` (2026-08-28).
- **Providers — parity inventory minimal fixes (Task 1.2)**: cross-provider
  behavior parity fixes for the request path (2026-08-28).
- **Agent — financial spend guard (Task 4.3)**: per-run spend limit with
  integer micro-USD metering under a single documented conservative policy
  pricing rate (policy placeholder, not provider rate data), strict
  `spent > limit` trip semantics, and `spend_limit_exceeded` terminal state
  (2026-08-29).
- **Agent — run streaming bridge (Task 5.1)**: `agent-run-event` frames
  streamed to the frontend, `StepRecorded` emission on the run thread,
  steps accordion UI, agent IPC commands, and conversation linkage (D50)
  (2026-08-30).
- **Agent — terminal/diff viewers & governance UI (Task 5.2)**: terminal and
  unified-diff viewers for tool output, runtime autonomy switch,
  pause/resume, budget extend, and startup orphan-run sweep (2026-08-30).
- **Agent — backend E2E suite (Task 6.1)**: end-to-end suite driving the real
  stack (file-backed SQLite, services, tool registry, event stream) with a
  deterministic scripted provider executor; no network, no new dependencies
  (2026-08-30).
- **CI**: runner-stall protection — the gates job now carries
  `timeout-minutes: 15` so stalled runners fail in minutes instead of hanging
  for ~20 (2026-08-30).

### Fixed

- **Approval emit-before-park race (Task 6.1 hotfix)**: once
  `ApprovalRequested` is emitted, a pending entry for that `call_id` always
  exists, so a concurrent resolve can never hit `NoPendingApproval` — the
  race is closed by construction (2026-08-30).
- **Approval fast-path stale pending cleanup (Task 6.2)**: switching the
  autonomy mode to auto-approve between `prepare_pending` and
  `request_approval` no longer leaves a stale pre-registered pending entry
  behind; a late `respond` for the stale id resolves nothing (2026-08-30).
- **Stress hardening (Task 6.2)**: in-crate stress suite proving sustained
  ≥250-turn runs, three cancellation paths (in-flight command, approval park,
  spend trip), ≥8 concurrent runs across conversations, mode-switch storms,
  budget-extend loops, exact spend accumulation, and duplicate-start
  rejection under concurrency.

## [0.1.0] — 2026-08-24

### Added

- Local-first MVP baseline: Tauri v2 desktop app with React 19 + TypeScript
  frontend and a Rust backend, SQLite persistence via `rusqlite`.
- Provider/model/credential integration with OS-keyring storage (API keys
  never touch SQLite, logs, or source).
- Conversation workspace, prompt library and search, attachments,
  import/export, settings with FR-012 validation, and FTS5 search indexes.
- Material 3 Expressive visual system across the workspace.

### Fixed

- MVP bug-fix sprint (BUG-001, BUG-003, BUG-004, BUG-005) and workspace
  sandbox hardening (2026-08-24/26).

> Note: tag `v0.3.0` exists remotely from the MVP era but carries no changelog entry; superseded by 1.0.0.

[1.2.0]: https://github.com/sshdw/Nexora/releases/tag/v1.2.0
[1.1.0]: https://github.com/sshdw/Nexora/releases/tag/v1.1.0
[1.0.2]: https://github.com/sshdw/Nexora/releases/tag/v1.0.2
[1.0.1]: https://github.com/sshdw/Nexora/releases/tag/v1.0.1
[1.0.0]: https://github.com/sshdw/Nexora/releases/tag/v1.0.0
[0.1.0]: https://github.com/sshdw/Nexora/releases/tag/v0.1.0
