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



Domain Layer (`src-tauri/src/domain/`)



↓



Infrastructure Layer (`src-tauri/src/infrastructure/` — SQLite, repositories, provider executors)



↓



SQLite / Local Storage / OS services (keyring, fs, dialogs)



AI providers are external services accessed only through the backend
(`infrastructure/providers/` executors behind the provider-independent
`application/execution.rs` boundary).



5\. Application Layers

The backend is five layers. Dependency direction is strictly downward:
commands → application → domain → infrastructure, with AI providers external
behind the executor boundary. Nothing below the commands layer handles Tauri IPC (commands, AppHandle, managed-state wiring); the sole exception is the provider transport's use of `tauri::async_runtime::spawn`; nothing above the infrastructure layer touches SQLite or the network
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

Domain Layer (`src-tauri/src/domain/`)

Responsible for:

business rules

entities

use cases

application logic

Independent from UI.

Infrastructure Layer (`src-tauri/src/infrastructure/`)

Responsible for:

database (`database.rs`: `MIGRATIONS` v1–v14, WAL + FK + busy-timeout
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

