This file is a **historical record**. MVP phases 0–9 completed in 0.1.0. Agent-era phases 4.1–6.2 were planned and tracked separately (see `docs/AGENT-4.3-DESIGN.md`, `docs/AGENT-5.1-DESIGN.md`, and CHANGELOG).

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

