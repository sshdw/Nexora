# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[1.0.0]: https://github.com/sshdw/Nexora/releases/tag/v1.0.0
[0.1.0]: https://github.com/sshdw/Nexora/releases/tag/v0.1.0
