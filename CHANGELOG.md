# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.3.1] — 2026-09-19

### Security

- Agent workspace guard widened beyond `C:\Windows`: `%SystemRoot%` on any drive,
  `Program Files`, `Program Files (x86)`, `ProgramData`, `Users`, `Windows.old` and
  `$Recycle.Bin`, the Unix system trees, UNC paths and drive roots are now rejected, and the
  stored root is re-canonicalized and re-checked on every use.
- Strict production CSP in `src-tauri/tauri.conf.json`: `script-src 'self'` with no
  `unsafe-inline` and no `unsafe-eval`, plus `object-src 'none'` and `base-uri 'self'`; the
  Vite dev server keeps a separate permissive `devCsp`.
- Attachment file paths are validated in the backend: the path must be absolute, is
  canonicalized and stored canonical, protected system locations, UNC paths and drive roots
  are rejected, only existing regular files are accepted, and the stored path is re-validated
  immediately before the file is read.

## [1.3.0] — 2026-09-13

### Added

- Agent workspace folder picker: sidebar folder button (`dialog.open`),
  header chip showing the current folder, 5-entry recent folders dropdown,
  and the current root in Settings. New settings `agent.workspace_root`
  (canonicalized, defaults to the pre-picker `agent_workspace` behavior) and
  `agent.workspace_recent` (max 5, JSON); guard rejects `C:\Windows`, drive
  roots, and non-existent paths; tool scope reads the setting root.
- Per-folder chat history: forward-only migration v6 adds
  `conversations.workspace_root TEXT NULL CHECK (length <= 1024)` (no FK;
  `NULL` for pre-picker rows); downgrade refusal unchanged.

## [1.2.3] — 2026-09-13

### Fixed

- OpenRouter shortlist re-gated per live smoke (chat + tools legs, status
  codes only): keep 4 (`inclusionai/ling-3.0-flash-fin:free`,
  `nvidia/nemotron-3.5-lightning:free`,
  `nvidia/nemotron-3-super-120b-a12b:free`,
  `cohere/north-mini-code:free` — chat 200, tools 200 agent-usable);
  dropped `minimax/minimax-m3:free`, `minimax/minimax-m2.7:free`,
  `z-ai/glm-5.2:free` (chat 404) and
  `nvidia/nemotron-3-ultra-550b-a55b:free` (no HTTP response twice);
  added 4 live-proven replacements
  (`nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`,
  `inclusionai/ling-3.0-flash-sante:free`,
  `inclusionai/ling-3.0-flash-vl:free`, `liquid/lfm-2.5-2.6b:free` —
  chat 200, tools 200). Default becomes
  `inclusionai/ling-3.0-flash-fin:free`; pricing unchanged.
- xKiro shortlist untouched: all 8 IDs chat 404 and no live-listed
  replacements proven, so the list stays but is flagged stale; keys alive
  (`/models` 200).

## [1.2.2] — 2026-09-13

### Fixed

- Gemini list replaced per live smoke (rotated key, chat + tools legs):
  keep 7 (`gemini-3.6-flash`, `gemini-3.1-flash-lite`,
  `gemini-3.1-pro-preview`, `gemini-flash-lite-latest`, `gemini-pro-latest`,
  `gemini-3.5-flash`, `gemini-3.5-flash-lite` — chat 200 keep, tools 200
  agent-usable, 429 stays); dropped `gemini-2.5-flash`,
  `gemini-2.5-flash-lite`, `gemini-2.5-pro` (404) and `gemini-flash-latest`
  (503). Default stays `gemini-3.6-flash`; pricing unchanged.

## [1.2.1] — 2026-09-12

### Fixed

- Compat shortlists re-gated by live smoke (xKiro 8, OpenRouter 8, NVIDIA
  NIM 5, OpenCode Zen 5): an ID stays listed iff a live POST to the
  provider's `chat/completions` endpoint returns chat 2xx for it, and is
  agent-usable iff the tools leg returns 2xx; 429-only IDs stay listed.
  Native OpenAI/Anthropic/Gemini lists unchanged.
- Chat sends now carry the shared 120s request timeout, so a stalled POST
  surfaces the existing network/timeout error instead of hanging.
- HTTP 402 (insufficient credits/quota) on the OpenAI-compatible path now
  surfaces its own message (top up or switch to a free-tier ID); Anthropic
  and Gemini 404 now map to invalid request, like OpenAI.
- Spend guard bills $0 for known-free model IDs (`:free` suffix, `-free`
  infix); everything else keeps the 5M/25M policy rate.

## [1.2.0] — 2026-09-05

### Added

- Provider wire verification: the four OpenAI-compatible shortlists now list
  smoke-gated keep-lists (an ID stays listed iff a live POST to the
  provider's `chat/completions` endpoint returns chat 2xx for it, and is
  agent-usable iff the tools leg returns 2xx; 429-only IDs stay listed);
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

[1.3.1]: https://github.com/sshdw/Nexora/releases/tag/v1.3.1
[1.3.0]: https://github.com/sshdw/Nexora/releases/tag/v1.3.0
[1.2.3]: https://github.com/sshdw/Nexora/releases/tag/v1.2.3
[1.2.2]: https://github.com/sshdw/Nexora/releases/tag/v1.2.2
[1.2.1]: https://github.com/sshdw/Nexora/releases/tag/v1.2.1
[1.2.0]: https://github.com/sshdw/Nexora/releases/tag/v1.2.0
[1.1.0]: https://github.com/sshdw/Nexora/releases/tag/v1.1.0
[1.0.2]: https://github.com/sshdw/Nexora/releases/tag/v1.0.2
[1.0.1]: https://github.com/sshdw/Nexora/releases/tag/v1.0.1
[1.0.0]: https://github.com/sshdw/Nexora/releases/tag/v1.0.0
[0.1.0]: https://github.com/sshdw/Nexora/releases/tag/v0.1.0
