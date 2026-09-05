# Agent E2E — Manual Smoke Checklist (Deferred UI Items for 6.2)

This document is the pre-release manual checklist for the deferred UI aspects of the agent stack that are **not** covered by the automated backend E2E suite (`src-tauri/src/application/agent/e2e.rs`). The backend suite drives the real SQLite (file-backed), `ConversationService`, `AgentRunHost`, `ToolRegistry` (real FS), settings, and event stream with a scripted provider executor. The items below require a running desktop app (`npm run tauri dev`) and human verification.

## How to run the automated suite

```sh
cargo test e2e                 # selects exactly the 10 e2e_* scenarios + gated smoke (ignored)
cargo test -- --ignored        # runs only the gated real-provider smoke (env-gated, safe-by-default)
NEXORA_E2E_REAL_PROVIDER=1 NEXORA_E2E_MODEL=gpt-5.6-terra cargo test -- --ignored
```

The gated smoke compiles in CI but never runs there (`#[ignore]` + `NEXORA_E2E_REAL_PROVIDER=1` guard, no secrets in logs).

## Manual UI checklist (6.2 pre-release)

Perform these with `npm run tauri dev` on a real build, covering both light and dark themes where relevant.

### 1. Accordion animation
- [ ] Steps accordion (AgentRunSteps) opens/closes with smooth height animation, no jank or content flash.
- [ ] Rapid open/close and switching conversations during a live run do not leave the accordion in a stuck or clipped state.
- [ ] Long step lists (tool calls + model turns + approvals) scroll correctly inside the accordion without overflow glitches.

### 2. Live-stream latency
- [ ] When an agent run is active, `StepRecorded` events appear within ~1s of the backend emitting them (no multi-second lag).
- [ ] `Finished(completed)` is always the last event; no flicker where steps appear after completion.
- [ ] Pausing and resuming a run reflects `Paused`/`Resumed` governance frames promptly in the UI.

### 3. Toggle UX
- [ ] Autonomy toggle (`agent.autonomy` = `supervised` / `semi_autonomous` / `full_autonomous`) updates the mode badge on the active run and persists across reloads.
- [ ] In `semi_autonomous`, a `write_file`/`execute_command` parks and shows an approval card; approving writes the file and denies leaves it absent.
- [ ] In `full_autonomous`, the same `write_file` executes without parking (no approval card).
- [ ] Pause/Resume button toggles correctly: paused run shows Paused state, Resume continues, Cancel works from every state (running, approval-parked, budget-parked).
- [ ] Budget park (iteration limit) shows the “Continue” affordance; extending continues to `Completed` with status `completed` (not `budget_exhausted`).

### 4. General
- [ ] Conversation list shows the agent run’s conversation with updated `updated_at` after both plain chat and agent completions.
- [ ] Reopening the app after a crash sweeps any orphaned `running` runs to `error` and rehydration lists runs/steps ordered by `seq`.
- [ ] No API keys or credential material appear in the UI, logs, or error toasts.

## Owner

Task 6.1 backend E2E is verified by the green `cargo test` suite; the ignored test is `e2e_real_provider_smoke`. This checklist is the only deferred manual gate for 6.2; keep it short and check it before release.

## 1.0 release gate

This manual checklist is the **1.0.0 release gate**: the Human confirms every
item above BEFORE the `v1.0.0` tag is created. Tagging happens at promote time
only — never as part of an implementation task. The automated counterpart of
the gate is the Task 6.2 stress suite (`cargo test stress_`).
