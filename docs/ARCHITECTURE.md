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



React UI



↓



Application Layer



↓



Rust Backend



↓



SQLite



↓



Local Storage



AI providers are external services accessed only through the backend.



5\. Application Layers

Presentation Layer



Responsible for:



user interface

navigation

rendering

input handling



Contains no business logic.



Application Layer



Responsible for:



application state

workflow

coordination

validation

request orchestration

Domain Layer



Responsible for:



business rules

entities

use cases

application logic



Independent from UI.



Infrastructure Layer



Responsible for:



database

filesystem

AI providers

operating system integration

configuration

6\. Data Flow

User Action



↓



Validation



↓



Application Logic



↓



Persistence / AI Request



↓



Result



↓



UI Update

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

OpenAI-compatible providers (xKiro, OpenRouter, NVIDIA NIM, OpenCode Zen) reuse `OpenAiExecutor` with a distinct endpoint (plus OpenRouter Referer/Title headers); they are not separate protocol implementations.



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

