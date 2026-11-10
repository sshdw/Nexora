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
