This file is a **historical record**. MVP phases 0–9 completed in 0.1.0. Agent-era phases 4.1–6.2 were planned and tracked separately (see `docs/AGENT-4.3-DESIGN.md`, `docs/AGENT-5.1-DESIGN.md`, and CHANGELOG).

Nexora 2.0 Phase 1 — Interrupted Run History (supplement; pending implementation)

Goal: preserve interrupted agent-run history as a distinct, non-executing terminal outcome (SRS §12.1).

Scope: transactional additive v15 interruption marker; lease-protected orphan-run marking; Rust/TypeScript history DTO and rehydration mapping; existing AgentRunSteps localized quiet presentation with retained steps; correction of existing activity/diagnostic classification, counts, and labels. Current implemented schema remains v14 until migration lands. Task recovery remains legacy; continuation, automatic work, tool policy, permissions, subagents, and normal run execution are excluded.

Dependencies: separate owner approval of the exact SRS, ARCHITECTURE, DATABASE, and ROADMAP amendments before source implementation.

Completion criteria: populated v14 → v15 preservation across every existing run status and linked steps; default `0` and 0/1 constraint; migration rollback and newer-schema refusal; orphan-only idempotent marking with live-instance protection; restart history/step rehydration; distinct interruption/error counts and diagnostics; regression coverage that existing error filters exclude flagged interruptions and retain unflagged actual/legacy errors; localized retained-history presentation with no failed tone, retry prompt, Continue, or automatic execution.

Purpose



This roadmap defines the implementation order of the Nexora MVP described in SRS.md.



The roadmap does not define product requirements. It only organizes implementation into sequential development phases.



Each phase contains:



Goal

Scope

Dependencies

Completion Criteria



No phase may introduce functionality outside the approved MVP.



Development Rules

Follow SRS requirements only.

Complete dependencies before starting dependent phases.

Every implementation task must be atomic.

Every completed phase must satisfy its completion criteria.

Features outside MVP require explicit approval.

Phase 0 — Project Foundation

Goal



Establish the technical foundation required for development.



Scope

Project initialization

Tauri configuration

React configuration

TypeScript configuration

Rust backend setup

SQLite initialization

Migration runner

Logging

Configuration loading

Development environment

Dependencies



None.



Completion Criteria

Project builds successfully.

Desktop application launches.

SQLite initializes correctly.

Migrations execute successfully.

Development environment is reproducible.

Phase 1 — Database \& Persistence

Goal



Implement persistent local storage.



Scope

Database connection

Repository layer

CRUD infrastructure

Transaction handling

Persistent settings

Data loading

Data saving

Dependencies



Phase 0



Completion Criteria

Persistent storage functions correctly.

Data survives restart.

CRUD operations pass testing.

Database integrity is preserved.

Phase 2 — Application Settings

Goal



Implement application configuration.



Scope

General settings

Appearance

Default model

Conversation preferences

Export preferences

Data management settings

Settings persistence

Dependencies



Phase 1



Completion Criteria



FR-012 satisfied.



Phase 3 — AI Providers

Goal



Implement provider-independent AI communication.



Scope

Provider management

Model selection

Credential management

Request execution

Response processing

Retry handling

Provider errors

Dependencies



Phase 1



Completion Criteria



FR-004



FR-014



FR-015 (provider-related behavior)



Phase 4 — Conversations

Goal



Implement conversation management.



Scope

Create conversation

Continue conversation

Rename

Archive

Restore

Delete

Conversation history

Message exchange

Failed request handling

Dependencies



Phase 1



Phase 3



Completion Criteria



FR-002



FR-003



FR-005



FR-006



Phase 5 — Prompt Library

Goal



Implement reusable prompts.



Scope

Create prompt

Edit prompt

Delete prompt

Insert prompt into conversation

Dependencies



Phase 4



Completion Criteria



FR-007



Phase 6 — Documents

Goal



Implement document-assisted conversations.



Scope

Attach files

Display attachments

Remove attachments

Validation of supported files

Dependencies



Phase 4



Completion Criteria



FR-008



Phase 7 — Local Search

Goal



Implement offline search.



Scope

Conversation search

Prompt search

Result navigation

Dependencies



Phase 4



Phase 5



Completion Criteria



FR-009



Phase 8 — Import \& Export

Goal



Implement data portability.



Scope

Export

Export conversations

Preserve message order

Import

Import supported files

Validation

Error reporting

Dependencies



Phase 4



Completion Criteria



FR-010



FR-011



Phase 9 — Data Management

Goal



Implement local data management.



Scope

Delete conversations

Delete prompts

Clear application data

Confirmation dialogs

Dependencies



Phase 4



Phase 5



Completion Criteria



FR-013



Phase 10 — Testing \& Polish

Goal



Prepare the MVP for release.



Scope

Functional testing

Offline testing

Performance verification

Error handling verification

Regression testing

Documentation verification

Bug fixing

Dependencies



All previous phases



Completion Criteria

All Critical requirements pass.

All High requirements pass.

No unresolved blocker remains.

MVP acceptance criteria are satisfied.

Critical Path

Foundation

&#x20;       ↓

Database

&#x20;       ↓

AI Providers

&#x20;       ↓

Conversations

&#x20;      ↙   ↘

Prompt   Documents

&#x20;    ↘      ↙

&#x20;     Search

&#x20;        ↓

&#x20;Import / Export

&#x20;        ↓

Data Management

&#x20;        ↓

Testing \& Polish

MVP Release Checklist



The MVP is complete when:



All Critical functional requirements are implemented.

All High functional requirements are implemented.

All acceptance criteria in SRS are satisfied.

Offline functionality behaves as specified.

Provider credentials are managed securely.

Local data persists correctly.

Import and export work correctly.

Local search functions correctly.

No approved MVP functionality is missing.


## M03.a transactional run-history journal

Depends on merged M01 and approval of the exact M03.a SRS, ARCHITECTURE, DATABASE and ROADMAP additions. Deliver the additive shared-core journal for new conversation-linked run start, recorded step, terminal outcome and orphan interruption, atomic with existing rows; typed conversation-scoped forward-cursor history reads; commit-before-notify identity; unchanged existing history presentation. Preserve legacy rows/IDs and M01 classification without backfill or execution replay. Validate ordered round-trip, transaction rollback, duplicate append/notification reconciliation, restart recovery, conversation isolation, cascade privacy and bounded sanitized output through production-reachable application/IPC paths. Record real startup/history smoke on supported OSes and concrete outstanding gaps. This atomic phase does not close the full M03 milestone or implement future question/queue/plan/child event contracts.

## M04 Stage 2 permission boundary and checkpoints

PR 2A follows Stage 1 completion (M04.a consent-floor repair plus M03 durable cursor consumption). Deliver all three persisted access profiles and effective parent/Plan ceilings, non-overridable deny-read guards, sensitive-write protection, segment/nested-shell classification with honest categories, complete approval identity invalidation, and command-specific shell consent with matching existing approval UI. Validate compound/obfuscated/platform shell cases, canonical/link boundary escapes, policy/profile/environment/posture changes, rules/group attempts to bypass ceilings, and truthful provenance through production-reachable paths. Do not enable Standard automatic writes without PR 2B checkpoints or automatic GeneralShell without verified Stage 7 isolation.

PR 2B delivers disk-backed content-addressed pre-mutation snapshots (`v17` `file_checkpoints` + `<app_data>/checkpoints/`), restart-safe turn/file ownership, conflict-checked one-click file/turn revert, and enables `Standard` automatic writes for ordinary workspace `write_file`/`edit_file` calls guarded by mandatory pre-mutation capture. Full M04 closes only after both PR units and their security/undo tests pass. Stage 7 separately delivers verified OS sandboxing and process-tree lifecycle; per-command shell fallback remains mandatory until verified isolation exists.

## Stage 12 — Runtime Stabilization, Auth/Catalog, 3-Mode Access, Web Tools & Local Models

Goal: eliminate confirmed runtime/data-flow bugs, restore ChatGPT OAuth reliability, update the provider catalog and capability cascade, unify access control to 3 modes (`Ask permissions`, `Auto-approve safe`, `Full access`), and deliver default-off Web Search/Fetch and Local Models.

Scope (PR 0 → PR 8):
- **PR 0 (Spec Amendment):** Approved updates to `docs/SRS.md`, `docs/ARCHITECTURE.md`, `docs/DATABASE.md` (no-migration note), `docs/ROADMAP.md`, and `OWNER-DECISIONS.md`.
- **PR 1 (Runtime, Streaming, Scroll, IPC Error & Terminal):** `formatIpcError(e)`, `<MessageMarkdown>` streaming + list markers, `48px` `isAtBottom` smart scroll + `↓` button + new-turn jump fix, `Attach files` in `Agent` mode + `+` popover focus fix, `ask_user_question` payload normalization, `PTY Terminal` cursor + `FitAddon` ↔ `ConPTY` `pty_resize` sync.
- **PR 2 (`Sign in with ChatGPT` OAuth + Plan Executor):** `dynamic_agent_client` primary flow with non-fatal `list_plan_models` + dual-schema/baseline fallback, `oaiapp_` + `app_` prefixes, `120s` `iat` leeway, RFC 6749 §5.1 `None` scope handling, and secret-free error variants.
- **PR 3 (Model Catalog & Auto-Metadata Cascade):** Deprecate/hide `xkiro` (and `opencode_zen` if live `/models` fails), update `SUPPORTED_MODELS` / `MODEL_RATES`, add `max_output_tokens`, fix Anthropic/Gemini/OpenAI-compat parsers, implement per-field cascade (`ManualOverride > Live API > BuiltInCatalog/FamilyRules > Unknown`), and add `test_provider_connection`.
- **PR 4 (Unified 3-Mode Access & `Full access`):** Unify `AccessProfile` + `AutonomyMode` onto 3 modes (`Ask permissions`, `Auto-approve safe`, `Full access`) with `Plan` orthogonal; explicit risk opt-in for `Full access`; preserve SSRF, keyring/redaction, `Deny` rules, budgets/timeouts, `Stop`, and prompt-injection marker pause.
- **PR 5 (Web Foundation & SSRF):** `application/web/{mod,safety,extract,fetch}.rs` reusing `mcp/url_safety.rs` DNS/IP pinning, self-contained HTML-to-Markdown converter, streaming/decompression caps, and SSRF tests (no model-facing tool exposed yet).
- **PR 6 (Web Tools, Search Backends & Minimal Settings Toggle):** `tools/web.rs`, `RiskClass::ExternalWebRead` (default-off, hidden in `Plan`/offline, host-bound consent in Modes 1–2, auto-approved in `Full access` with injection-marker pause), DDG keyless + Tavily/Brave/Exa keyring + SearXNG, `src/lib/ipc/web.ts`.
- **PR 7 (Local Models Foundation):** `ollama`, `lmstudio`, `llama_cpp`, `local_openai_compatible` identities, keyless loopback-only execution, in-memory discovery/cache, local timeouts, mock HTTP tests.
- **PR 8 (Local Models Wiring & Smoke):** End-to-end `list_all_model_capabilities` → `mergeConnectionModels` → `buildConnectionGroups`, `ContextRing` wiring, minimal local runtime connection UI, and isolated `APPDATA` smoke verification.

Dependencies: Stage 2 M04 complete; owner sign-off on PR 0 specifications.

Completion Criteria: all 9-step verification gates, `run-smart-gate.ps1`, `npm run lint`, Independent Reviewer M04/security checks, and isolated `APPDATA` live smoke tests pass for PR 1–8.

