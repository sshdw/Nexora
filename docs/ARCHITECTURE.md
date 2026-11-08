Nexora Architecture

1\. Purpose



This document defines the technical architecture of Nexora.



It describes the system structure, major components, data flow, design principles, constraints, and technical risks.



It does not define development workflow, AI responsibilities, or project management.



2\. Goals



The architecture must be:



local-first

modular

maintainable

predictable

fast

secure

easy to extend without large refactoring

3\. Technology Stack

Desktop

Tauri v2

Frontend

React

TypeScript

Backend

Rust

Database

SQLite

4\. High-Level Architecture

User



↓



React UI (TypeScript, `src/`)



↓ (Tauri IPC — typed `invoke` commands / streamed events)



Commands Layer (`src-tauri/src/commands/` — the IPC boundary)



↓



Application Layer (`src-tauri/src/application/` — use cases, agent subsystem)



↓



Infrastructure Layer (`src-tauri/src/infrastructure/` — SQLite, repositories, provider executors)



↓



SQLite / Local Storage / OS services (keyring, fs, dialogs)



AI providers are external services accessed only through the backend
(`infrastructure/providers/` executors behind the provider-independent
`application/execution.rs` boundary).



5\. Application Layers

Nexora 2.0 Phase 1 — Interrupted Run History

Startup recovery remains in `nexora_core::storage::lifecycle::sweep_orphans`,
called from desktop startup under the existing `InstanceLease`. Only a
lease holder that establishes no live peer may sweep persisted `running`
runs. The repository update atomically sets `interrupted = 1` together
with the existing SQL terminal `status = 'error'`, fixed shutdown message,
`finished_at = max(unixepoch(), started_at)`, and child-row `total_steps`
recount. It preserves the run and all step rows and is idempotent. The
existing task sweep remains unchanged and separate.

The v15 migration is additive and transactional (DATABASE §5, §7.8).
SQL retains its existing status CHECK. Rust `AgentRun` and its TypeScript
IPC counterpart expose `interrupted: boolean` alongside the raw persisted
`status`; repository read/list/pagination paths populate it. History
rehydration derives the user status `interrupted` when the flag is true,
otherwise retaining the existing status. Activity summaries apply the same
classification without changing normal execution events or run controls.
No legacy error is inferred or backfilled as interrupted.

Diagnostics `error_runs` counts `status = 'error' AND interrupted = 0`.
The existing `crashed_runs` IPC field retains its name for compatibility,
but lists only explicitly flagged interrupted runs, with its existing
metadata-only payload, ordering, and limit; it no longer infers interruption
from error text. Its localized presentation describes interruptions rather
than proven crashes. All-run, step, and spend totals retain these rows.
The existing AgentRunSteps presentation and its rehydration/label helpers
show interrupted history as a quiet terminal state with retained steps and
no Continue, retry prompt, pending approval, or automatic work. Existing
activity/system labels receive only the corrections needed for this
classification. This phase adds no continuation endpoint or execution path.

The backend is three layers. Dependency direction is strictly downward:
commands → application → infrastructure, with AI providers external
behind the executor boundary and the shared, shell-free core
`crates/nexora-core` (error/exit taxonomy, allow-listed config, storage
foundation, provider-execution boundary types) below both adapters.
The previously documented `domain/` layer was deleted on 2026-10-31 by
owner decision (NEX-ARCH-001 / AC-201): it had never contained code —
entities are repository rows and policy lives in `application/` and
`nexora_core` — so the truthful description is three layers, not four.
Nothing below the commands layer handles Tauri IPC (commands, AppHandle, managed-state wiring); the sole exception is the provider transport's use of `tauri::async_runtime::spawn`; nothing above the infrastructure layer touches SQLite or the network
directly.

Commands / IPC Layer (`src-tauri/src/commands/`)

Responsible for:

the Tauri IPC boundary (`#[tauri::command]` handlers registered in
`lib.rs::run` via `invoke_handler`)

argument validation and error mapping at the boundary (secret-free,
classified `CommandError`)

cloning managed state (`Database`, `AgentRunRegistry`, `TaskRegistry`,
`TerminalRegistry`, `ConfirmationRegistry`) into `spawn_blocking` / spawned threads

streaming run and task events back to the frontend (`agent-run-event`
frames, task events)

Contains no business logic: every handler delegates to the application
layer. Representative surfaces: `commands/agent.rs` (start / cancel /
pause / resume / extend / set-mode / resolve-approval / runs / steps /
inspect / spend-dashboard / activity-feed / permission rules),
`commands/tasks.rs` (create / list / update / delete / start / stop /
locks / move), plus the conversation, prompt, attachment, provider,
settings, workspace, import/export, privacy, debt, terminal, git, and
system surfaces registered alongside them in `lib.rs`.

Application Layer (`src-tauri/src/application/`)

Responsible for:

application state

workflow

coordination

validation

request orchestration

Houses the agent subsystem (`application/agent/`): the deterministic ReAct
`runner`, the `service` run bridge (dedicated threads, `AgentRunRegistry`,
conversation linkage), the `tasks` manager and bounded plan → act →
verify → report loop (`TaskRegistry`), the three-tier approval gate
(`approval`), autonomy modes and presets (`agent::approval::AutonomyMode`, parsed/wired via `service::parse_autonomy_mode`/`resolve_autonomy_mode`),
permission evaluation (`permissions`), spend/budget enforcement and views
(`budget`, `spend`), governance checkpoints and audit log (`governance`,
`control`, `lifecycle`), roles and pipeline stages (`roles`, `pipeline`),
context assembly and compaction (`assembly`, `compaction`), run-scoped
snapshots (`snapshots`), injection hardening (`injection`), self-audit
(`self_audit`), the read-only inspector (`inspect`), activity summaries
(`activity`), persistence recording (`persistence`), and the native
workspace tools (`tools/`). The provider-independent execution boundary
(`application/execution.rs`) also lives here (see §7).

Infrastructure Layer (`src-tauri/src/infrastructure/`)

Responsible for:

database (`crates/nexora-core/src/storage/database.rs`, re-exported by
`src-tauri/src/infrastructure/database.rs`: `MIGRATIONS` v1–v14 currently;
Phase 1 target v15 pending implementation, WAL + FK + busy-timeout
pragmas; `Database` managed state)

repositories (`repository/`: per-table query discipline, explicit
`updated_at` writes)

AI provider executors (`providers/`: protocol implementations behind the
`application/execution.rs` boundary)

filesystem

operating system integration

configuration

logging (`logging.rs`, initialized first in `lib.rs::run`)

Providers (external)

Responsible for:

provider selection

request execution

response normalization

timeout handling

retry policy

error propagation

Covered in detail in §7; the application never depends on
provider-specific behavior.

6\. Data Flow

User Action (React UI, `src/`)



↓ (Tauri `invoke` with typed args)



Commands Layer (validate, map errors, acquire managed state)



↓



Application Logic (use case / agent subsystem: runner, approvals, budgets)



↓



Persistence / AI Request (repositories / provider executors)



↓



Result (+ streamed `agent-run-event` / task-event frames for long-running work)



↓



UI Update

Presentation (React UI) holds no business logic: user interface,
navigation, rendering, input handling.

7\. AI Provider Layer



The AI layer is provider-independent.



Responsibilities:



provider selection

request execution

response normalization

timeout handling

retry policy

error propagation



The application should not depend on provider-specific behavior.



The provider-independent boundary (`application/execution.rs`) carries the
message model shared by every executor: `AiMessage` (role, text content,
attachments) plus the native tool round-trip fields — `tool_calls`
(`Vec<ToolCall>`, non-empty only on an assistant agent turn) and `tool_result`
(`Option<AiToolResult>`, present only on the `AiRole::Tool` variant). The
`Tool` role exists strictly for in-flight agent requests: tool observations
are returned to the provider in its native shape (Gemini
`functionCall`/`functionResponse` with thought-signature round-trip, OpenAI
`tool_calls`/`role: "tool"`, Anthropic `tool_use`/`tool_result`); it is never
persisted (DATABASE.md §7.2 remains user/assistant/system). `ToolCall.thought_signature`
is a provider-opaque reasoning signature (Gemini 3): pure pass-through — it is
echoed verbatim on the next request, and never logged, never persisted, never
parsed. The boundary's `ExecutorError` carries classified failure categories
(network, rate limit with the provider's Retry-After hint, provider unavailable,
rejected credential, invalid request, unexpected response) — still secret-free,
still no response-body reads, still no retry logic.

OpenAI-compatible providers (xKiro, OpenRouter, NVIDIA NIM, OpenCode Zen) reuse `OpenAiExecutor` with a distinct endpoint (plus OpenRouter Referer/Title headers); they are not separate protocol implementations. HTTP 404 from a compatible endpoint maps to invalid request (model/route missing) instead of the opaque catch-all failure.



8\. Database Layer



SQLite is the single source of truth.



Responsibilities:



persistent storage

transactional consistency

local performance

migration support



Schema details belong in DATABASE.md.



9\. Configuration



Configuration must be centralized.



Examples:



application settings

provider configuration

feature flags

logging configuration



Secrets must never be hardcoded.



10\. Error Handling



Errors must be:



classified

logged

recoverable whenever possible

shown to users in a clear form



Unexpected failures must not corrupt user data.



11\. Logging



Logging should support:



debugging

diagnostics

error investigation



Sensitive information must never be written to logs.



12\. Security



Architecture principles:



local-first

least privilege

secure secret storage

input validation

output sanitization



API keys must never be embedded in source code.



13\. Performance



Targets:



fast startup

responsive UI

efficient database access

minimal memory usage

non-blocking long-running operations

14\. Design Principles



The project follows these principles:



Separation of Concerns

Single Responsibility

Composition over inheritance

Explicit dependencies

Low coupling

High cohesion

Predictable behavior

15\. Constraints



The architecture must avoid unnecessary complexity.



Do not introduce:



distributed systems

cloud synchronization

background services without justification

unnecessary abstractions

unused infrastructure

16\. Future Evolution



Future capabilities should be added through new modules rather than rewriting existing ones.



Backward compatibility should be preserved whenever practical.



Architecture changes require explicit review before implementation.

