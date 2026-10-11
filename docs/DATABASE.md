 # Nexora Database Architecture Specification

## 1. Purpose

This document defines the complete SQLite database architecture for the Nexora MVP. The database is the single source of truth for all local persistent state, responsible for storing conversations, messages, prompts, attachments, provider metadata, application settings, agent runs and steps, permission rules, agent tasks, usage counts, debt items, and task locks. It enables offline-first operation, local full-text search, and data portability as required by SRS.md.

**Reference:** ARCHITECTURE.md, Section 8 (Database Layer).

---

## 2. Design Principles

- **Local-first**: All user data resides in a single local SQLite file. No cloud synchronization.
- **Offline-first**: The schema supports full functionality without network connectivity (FR-015).
- **Ownership**: The database file is self-contained and portable; the user retains full ownership.
- **Integrity**: Foreign keys, CHECK constraints, and explicit transactions prevent invalid states and orphaned data.
- **Consistency**: WAL mode provides ACID compliance and allows concurrent reads during writes.
- **Security**: Sensitive credentials are explicitly excluded from the database; only non-sensitive metadata and user content are stored.
- **Performance**: Indexes are limited to columns required by functional queries; no speculative indexes.
- **Maintainability**: The schema avoids unnecessary abstraction, JSON, and nullable complexity.

---

## 3. Database Overview

### SQLite Assumptions
SQLite is used as an embedded, serverless, single-file database. This aligns with ARCHITECTURE.md's local-first constraint and eliminates network dependencies.

### WAL Mode
Write-Ahead Logging is enabled.  

```sql
PRAGMA journal_mode=WAL;
```

**Rationale:** WAL allows the UI to read conversation history and perform searches without blocking on write operations (message insertion, settings updates). This is essential for a responsive desktop experience.

### Foreign Keys
Foreign key enforcement is enabled.  
**Rationale:** Ensures referential integrity between conversations, messages, attachments, and providers. Prevents orphaned messages and attachments.

### Transactions
All multi-statement operations use explicit transactions.  
**Rationale:** Guarantees atomicity. A conversation cannot be partially created, and an import cannot result in a half-populated database.

### JSON Usage
JSON is not used as queryable structure in any column.
**Rationale:** All MVP requirements are satisfied with a strictly relational schema. The sole exception is opaque payload text: `agent_steps.arguments` stores the raw provider-supplied JSON arguments verbatim (v4), and `agent_tasks.report` / `agent_task_steps.result` store free-form text. These columns are never parsed by SQL (no `json1` functions, no JSON indexes); provider-specific non-sensitive data does not require schema-flexible storage in the MVP.

### FTS5 Usage
FTS5 virtual tables are used for full-text search.  
**Rationale:** Required by FR-009 for offline local search across conversations and prompts. FTS5 provides efficient tokenization and ranking without external dependencies.

---

## 4. Schema Versioning

Schema changes are tracked in the `schema_version` table. Each migration inserts exactly one new row containing a monotonically increasing version number and an application timestamp. Version numbers are unique and strictly increasing. The current schema version is determined by the maximum version number present (`MAX(version)`), which therefore always represents the current schema version. The current schema version is **v14**.

**Rationale:** This provides an unambiguous, ordered migration path and prevents the application from running against an unrecognized schema version.

---

## 5. Migration Strategy

Migrations follow a forward-only, incremental philosophy. Each migration is atomic and versioned. The application refuses to start if the database file's schema version is newer than the application's known migration set, preventing backward incompatibility. Migrations are executed within a transaction; if any step fails, the entire migration rolls back, preserving database integrity. No destructive data changes occur without explicit validation.

v5 introduces a validated rebuild pattern for `agent_runs`: when a `CHECK` widening cannot be expressed with `ALTER TABLE ADD COLUMN`, the migration rebuilds the table (`CREATE TABLE agent_runs_new` -> copy -> `DROP TABLE agent_runs` -> `RENAME`) with `PRAGMA foreign_keys=OFF` applied before the transaction and restored to `ON` after (SQLite cannot toggle `foreign_keys` inside a transaction). This pattern is limited to v5; all other migrations remain byte-identical plain transactions.

v6 adds the `conversations.workspace_root` column as a plain additive `ALTER TABLE ADD COLUMN` transaction: forward-only, with no table rebuild, no foreign key, and no backfill, so pre-picker rows keep `NULL`.

v7 adds the `permission_rules` table (ordered, revocable tool-permission rules plus a seeded `coding`/`execute_command`/`ask` row) and rebuilds `agent_steps` to gain the provenance columns `rule_id` (`REFERENCES permission_rules(id) ON DELETE SET NULL`), `group_key`, and `decided_by` (existing rows keep `NULL`).

v8 adds the task-manager tables `agent_tasks` and `agent_task_steps` (new tables only, no rebuild).

v9 adds the local-only `usage_ledger` counts table (no text content column by construction).

v10 adds the `debt_items` backlog table.

v11 adds the `task_locks` kanban edit-lock table (`task_id` PK, expiring tokens).

v12 replaces the v3 `updated_at` touch triggers with monotonic bodies (`max(unixepoch(), created_at)`), so rows with future `created_at` no longer fail their next update against the `updated_at >= created_at` CHECK. Drop + recreate; no row data touched.

v13 adds cross-column invariants via four table rebuilds (the v5/v7 precedent): `messages` gains `UNIQUE (conversation_id, id)`; `attachments` gains a composite `FOREIGN KEY (conversation_id, message_id) REFERENCES messages (conversation_id, id) ON DELETE CASCADE` (pre-existing cross-conversation rows repaired to draft `message_id = NULL`); `agent_tasks` gains `CHECK (current_step <= total_steps)` (over-advanced rows clamped); `agent_runs` gains `CHECK (status = 'running' OR finished_at IS NOT NULL)` and `CHECK (finished_at IS NULL OR finished_at >= started_at)` (terminal rows without finish time stamped with `started_at`; `total_steps` normalized to the child-row count).

v14 adds the nullable `agent_steps.input_tokens` / `output_tokens` columns as plain additive `ALTER TABLE ADD COLUMN` transactions (no rebuild, no FK toggle); `NULL` for pre-existing rows and usage-less turns.

---

## 6. Entity Relationship Overview

| Entity | Ownership | Lifecycle | Relationships |
|--------|-----------|-----------|---------------|
| **Conversation** | User | Created, renamed, archived, restored, deleted | 1:N Message; 1:N Attachment (draft) |
| **Message** | Conversation | Created, immutable after creation | N:1 Conversation; N:1 Provider (optional) |
| **Prompt** | User | Created, edited, deleted | Standalone |
| **Attachment** | Conversation / Message | Created with conversation (draft), linked to message on send, deleted with conversation or message | N:1 Conversation; N:1 Message (optional) |
| **Provider** | User | Added, removed | Referenced by Message |
| **AppSetting** | Application | Updated throughout use | Standalone |
| **SchemaVersion** | Application | Inserted during migrations | Standalone |
| **AgentRun** | Application | Created at agent run start, finalized at run termination | 1:N AgentStep; N:1 Conversation (optional) |
| **AgentStep** | AgentRun | Appended during the run, immutable after insertion | N:1 AgentRun; N:1 PermissionRule (optional, nullable) |
| **PermissionRule** | User | Created, deleted | Referenced by AgentStep (optional, nullable) |
| **AgentTask** | User | Created, updated, deleted; executed via bounded run loop | 1:N AgentTaskStep; N:1 Conversation (optional); N:1 AgentRun (optional, latest step run) |
| **AgentTaskStep** | AgentTask | Created with task plan, updated during execution | N:1 AgentTask; N:1 AgentRun (optional, own step run) |
| **UsageLedgerRow** | Application | Incremented per event-day, pruned past 90 days | Standalone (counts only, no content) |
| **DebtItem** | User / Audit import | Created, triaged, deleted | Standalone |
| **TaskLock** | Application | Acquired, heartbeated, released/expired | 1:1 AgentTask |

---

## 7. TABLE DESIGN

### 7.1 conversations

**Purpose:** Stores AI conversation entities.  
**Related FR:** FR-002, FR-005, FR-006, FR-013

**CRUD:**
- **Create:** `INSERT`. Triggered by user action "new conversation".
- **Read:** `SELECT` by `id`; `SELECT` filtered by `status`; `SELECT` via FTS5.
- **Update:** `UPDATE` of `title` (rename), `status` (archive/restore). `updated_at` is maintained by trigger.
- **Delete:** `DELETE` by `id`. Cascades to messages and attachments.

**Deletion Behavior:** Hard delete. User-owned. `ON DELETE CASCADE` to `messages` and `attachments` (via `conversation_id`).  
**Rationale:** FR-013 requires that users can permanently remove conversations. A conversation and its entire history are a single unit of deletion.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. Stable identifier independent of user-editable content. |
| title | Human-readable name | TEXT | NO | `'Untitled Conversation'` | `length(title) > 0 AND length(title) <= 500` | None | No | FR-002, FR-006 |
| status | Archive state | TEXT | NO | `'active'` | `status IN ('active', 'archived')` | None | No | FR-006 |
| created_at | Creation timestamp | INTEGER | NO | Current Unix timestamp | `created_at > 0` | None | No | Implementation Decision. Preserves chronological ordering required by FR-005. |
| updated_at | Last modification timestamp | INTEGER | NO | Current Unix timestamp | `updated_at >= created_at` | None | No | Implementation Decision. Tracks recency for FR-006 active conversation listing and sorting. |
| workspace_root | Denormalised workspace scope marker | TEXT | YES | NULL | `workspace_root IS NULL OR length(workspace_root) <= 1024` | None | No | Migration v6 (per-folder history). The canonical root is owned by the `agent.workspace_root` setting, so this column is a scope marker, not a foreign key; `NULL` for pre-picker rows. |

---

### 7.2 messages

**Purpose:** Stores individual messages within conversations.  
**Related FR:** FR-003, FR-004, FR-005

**CRUD:**
- **Create:** `INSERT` on user send or upon receipt of an assistant response.
- **Read:** `SELECT` by `conversation_id` ordered by `created_at`.
- **Update:** None. Messages are immutable after creation.
- **Delete:** `DELETE` by `id` (cascades to linked attachments), or `CASCADE` from conversation deletion.

**Deletion Behavior:** Hard delete. Owned by conversation. `ON DELETE CASCADE` from `conversations`. `ON DELETE CASCADE` to `attachments` (where `message_id` is set).  
**Rationale:** FR-013 requires data removal. Messages are part of conversation history and do not outlive their conversation.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| conversation_id | Owning conversation | INTEGER | NO | None | `conversation_id > 0` | conversations.id CASCADE | No | FR-002, FR-005 |
| role | Message author type | TEXT | NO | None | `role IN ('user', 'assistant')` | None | No | FR-003 |
| content | Message text | TEXT | NO | None | `length(content) > 0` | None | No | FR-003 |
| provider_id | AI provider used | INTEGER | YES | NULL | `provider_id IS NULL OR provider_id > 0` | providers.id SET NULL | No | FR-004 |
| model_name | Specific model used | TEXT | YES | NULL | `length(model_name) <= 200` | None | No | FR-004. Implementation Decision: records the selected model in persisted history. |
| created_at | Creation timestamp | INTEGER | NO | Current Unix timestamp | `created_at > 0` | None | No | Implementation Decision. Preserves strict chronological order within a conversation per FR-005. |

**v13 invariant:** `UNIQUE (conversation_id, id)` so the attachment link can reference the `(conversation, message)` pair (see §7.4 composite FK). Rows are otherwise identical to v1.

**Note on failed requests:** Failed assistant responses are not persisted as messages. FR-003 requires that failed requests display an error in the UI; this is handled at the application layer. Persisting failed state would require either nullable `content` or an additional status column, both of which introduce unnecessary complexity beyond the MVP requirements.

---

### 7.3 prompts

**Purpose:** Stores reusable prompt templates.  
**Related FR:** FR-007

**CRUD:**
- **Create:** `INSERT` by user.
- **Read:** `SELECT` all; `SELECT` by `id`; `SELECT` via FTS5.
- **Update:** `UPDATE` of `title` and `content`. `updated_at` is maintained by trigger.
- **Delete:** `DELETE` by `id`.

**Deletion Behavior:** Hard delete. User-owned. No cascade.  
**Rationale:** FR-007 and FR-013. Prompts are standalone entities.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| title | Prompt name | TEXT | NO | None | `length(title) > 0 AND length(title) <= 200` | None | No | FR-007 |
| content | Prompt text | TEXT | NO | None | `length(content) > 0 AND length(content) <= 10000` | None | No | FR-007 |
| created_at | Creation timestamp | INTEGER | NO | Current Unix timestamp | `created_at > 0` | None | No | Implementation Decision. Supports FR-007 library organization. |
| updated_at | Last edit timestamp | INTEGER | NO | Current Unix timestamp | `updated_at >= created_at` | None | No | Implementation Decision. Tracks edits per FR-007. |

---

### 7.4 attachments

**Purpose:** Tracks local files attached to AI requests.  
**Related FR:** FR-008

**CRUD:**
- **Create:** `INSERT` when user attaches a file (`message_id` is `NULL`). `UPDATE` of `message_id` from `NULL` to a message id when the message is sent.
- **Read:** `SELECT` by `conversation_id` where `message_id IS NULL` (draft attachments); `SELECT` by `message_id` (historical attachments).
- **Update:** `UPDATE` of `message_id` only, linking a draft attachment to its message at send time. No other updates.
- **Delete:** `DELETE` by `id` (user removes before sending); `CASCADE` from conversation or message deletion.

**Deletion Behavior:** Hard delete. Owned by conversation. `ON DELETE CASCADE` on `conversation_id`. `ON DELETE CASCADE` on `message_id` for historically linked attachments.  
**Rationale:** FR-008 requires that attachments be visible before message submission and removable before sending. This requires a draft state (`message_id IS NULL`) where the attachment belongs to the conversation but not yet to a message. When the message is sent, the attachment is linked. When the conversation is deleted, all its draft and historical attachments are removed per FR-013.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| conversation_id | Owning conversation | INTEGER | NO | None | `conversation_id > 0` | conversations.id CASCADE | No | FR-008. Implementation Decision: required to support pre-submission attachment visibility. |
| message_id | Associated message | INTEGER | YES | NULL | `message_id IS NULL OR message_id > 0` | messages.id CASCADE | No | FR-008. Implementation Decision: `NULL` represents the draft state before submission; set when the message is created. |
| file_name | Display name | TEXT | NO | None | `length(file_name) > 0 AND length(file_name) <= 255` | None | No | FR-008 |
| file_path | Absolute filesystem path | TEXT | NO | None | `length(file_path) > 0` | None | No | FR-008 |
| file_size_bytes | File size | INTEGER | YES | NULL | `file_size_bytes >= 0` | None | No | Implementation Decision. Required for FR-008 validation of supported file limits. |
| mime_type | Media type | TEXT | YES | NULL | `length(mime_type) <= 127` | None | No | Implementation Decision. Required for FR-008 validation of supported file types. |

**v13 invariant:** alongside the single-column FKs, a composite `FOREIGN KEY (conversation_id, message_id) REFERENCES messages (conversation_id, id) ON DELETE CASCADE` guarantees an attachment can only link a message of its own conversation. The single-column conversation FK still guards draft rows (`message_id IS NULL` skips the composite FK under SQLite NULL semantics). Pre-existing cross-conversation rows are repaired to draft (`message_id = NULL`) during the v13 rebuild.

---

### 7.5 providers

**Purpose:** Stores non-sensitive metadata for configured AI providers.  
**Related FR:** FR-004, FR-014

**CRUD:**
- **Create:** `INSERT` when user configures a provider.
- **Read:** `SELECT` by `id`; `SELECT` by `name`.
- **Update:** None in MVP. Provider metadata is static after creation.
- **Delete:** `DELETE` by `id`. `SET NULL` on `messages.provider_id`.

**Deletion Behavior:** Hard delete. User-owned. `ON DELETE SET NULL` on `messages.provider_id`.  
**Rationale:** FR-014 allows removal of provider configuration. FR-005 requires that message history not be destroyed when a provider is removed.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| name | Internal identifier | TEXT | NO | None | `length(name) > 0 AND length(name) <= 100` | None | Yes | Implementation Decision. Used as the keyring entry namespace key and application logic identifier. |
| display_name | User-facing label | TEXT | NO | None | `length(display_name) > 0` | None | No | FR-004 |

**Removed from previous draft:** `is_enabled`, `config_json`, `created_at`, `updated_at`.  
**Rationale:** `is_enabled` is not required by SRS; a provider's availability is determined by the presence of configuration and credentials. `config_json` introduced unnecessary schema flexibility; provider-specific non-sensitive parameters (e.g., model lists, timeout values) are either hardcoded or managed by the application layer in the MVP. Timestamps were not justified by any functional requirement for this table.

---

### 7.6 app_settings

**Purpose:** Stores application configuration as key-value pairs.  
**Related FR:** FR-012

**CRUD:**
- **Create:** `INSERT` on first use of a setting key.
- **Read:** `SELECT` by `key`.
- **Update:** `UPDATE` of `value`.
- **Delete:** `DELETE` by `key` (e.g., reset to default).

**Deletion Behavior:** Hard delete. Application-managed. No cascade.  
**Rationale:** FR-012, FR-013.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| key | Setting identifier | TEXT | NO | None | `length(key) > 0 AND length(key) <= 200` | None | PK | FR-012 |
| value | Setting value | TEXT | YES | NULL | `length(value) <= 10000` | None | No | FR-012 |

**Simplification from previous draft:** Removed the surrogate `id` column; `key` is the primary key.  
**Rationale:** A settings key is inherently unique. A separate surrogate key added unnecessary indirection.

---

### 7.7 schema_version

**Purpose:** Tracks applied database schema migrations.  
**Related FR:** None (infrastructure)

**CRUD:**
- **Create:** `INSERT` during migration.
- **Read:** `SELECT MAX(version)`.
- **Update:** None.
- **Delete:** None (append-only).

**Deletion Behavior:** Append-only. Never deleted.  
**Rationale:** Migration audit trail.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| version | Migration number | INTEGER | NO | None | `version > 0` | None | PK | Implementation Decision. |
| applied_at | Application timestamp | INTEGER | NO | Current Unix timestamp | `applied_at > 0` | None | No | Implementation Decision. |

### 7.8 agent_runs

**Purpose:** Persists an agent run (the multi-step ReAct loop with governance) when opt-in persistence is attached to the runner. One row per run.  
**Related FR:** None (agent roadmap, Task 4.2; D12)

**CRUD:**
- **Create:** `INSERT` at run start with `status = 'running'` (only when a run recorder is attached).
- **Read:** `SELECT` by `id`; `SELECT` ordered by `started_at` for run history; `SELECT` by `conversation_id` for the runs of one conversation.
- **Update:** Terminal fields only — `status`, `finished_at`, `total_steps`, `final_content`, `error` — at run termination.
- **Delete:** `DELETE` by `id`. Cascades to agent_steps.

**Deletion Behavior:** Hard delete. Application-managed (no user UI in Task 4.2). `ON DELETE CASCADE` to agent_steps; also cascade-deleted when its conversation is removed (optional link, D50 privacy doctrine).  
**Rationale:** A run and its step records form a single unit of deletion (D12); a conversation and its agent history form a single unit of privacy (Nexora privacy doctrine, D50).

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| conversation_id | Owning conversation (NULL until the M5 IPC layer wires runs to conversations) | INTEGER | YES | NULL | None | conversations.id | No | Agent roadmap (Task 4.2). `ON DELETE CASCADE`. |
| model | Provider model name for the run | TEXT | NO | None | `length(model) > 0` | None | No | Agent roadmap (Task 4.2). Never a credential. |
| mode | Autonomy mode at run start | TEXT | NO | None | `mode IN ('supervised', 'semi_autonomous', 'full_autonomous')` | None | No | Agent roadmap (Task 4.2). |
| status | Run state | TEXT | NO | `'running'` | `status IN ('running', 'completed', 'cancelled', 'budget_exhausted', 'spend_limit_exceeded', 'error')` | None | No | Agent roadmap (Task 4.2/4.3). |
| started_at | Start timestamp | INTEGER | NO | Current Unix timestamp | `started_at > 0` | None | No | Implementation Decision. |
| finished_at | Termination timestamp | INTEGER | YES | NULL | `finished_at > 0` | None | No | Agent roadmap (Task 4.2). |
| total_steps | Number of recorded steps | INTEGER | NO | 0 | `total_steps >= 0` | None | No | D12. |
| final_content | Final assistant text (terminal `completed` only) | TEXT | YES | NULL | None | None | No | Agent roadmap (Task 4.2). |
| error | Classified error text (terminal `error` only) | TEXT | YES | NULL | None | None | No | Agent roadmap (Task 4.2). |
| spent_micro_usd | Total spend at finalize in micro-USD | INTEGER | YES | NULL | `spent_micro_usd IS NULL OR spent_micro_usd >= 0` | None | No | Task 4.3. Policy default pricing; stored only when persisted. |
| limit_micro_usd | Per-run spend limit in micro-USD | INTEGER | YES | NULL | `limit_micro_usd IS NULL OR limit_micro_usd >= 0` | None | No | Task 4.3. NULL = no limit (opt-in). |

**v13 invariants:** `CHECK (status = 'running' OR finished_at IS NOT NULL)` (a terminal status requires a finish time) and `CHECK (finished_at IS NULL OR finished_at >= started_at)`. Pre-existing terminal rows with `finished_at IS NULL` are stamped with `started_at` during the rebuild, and `total_steps` is normalized to the child-row count.

**Implementation Note:** `conversation_id` is stored as NULL until the Task 5.1 IPC layer begins passing the owning conversation to the runner (D50). The column and its CASCADE exist from this migration so the privacy doctrine holds from day one.

---

### 7.9 agent_steps

**Purpose:** Structured step records of an agent run (D12): one row per model turn marker, per tool call, and per parked approval decision.  
**Related FR:** None (agent roadmap, Task 4.2; D12)

**CRUD:**
- **Create:** `INSERT` (append) during the run, in step order, only when a run recorder is attached.
- **Read:** `SELECT` by `run_id` ordered by `seq`.
- **Update:** None. Immutable after insertion.
- **Delete:** Only via `ON DELETE CASCADE` from agent_runs.

**Deletion Behavior:** Never deleted directly. `ON DELETE CASCADE` from agent_runs.  
**Rationale:** Step records exist only while their run exists (D12).

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| run_id | Owning run | INTEGER | NO | None | None | agent_runs.id | No | D12. `ON DELETE CASCADE`. |
| seq | 1-based step sequence within the run | INTEGER | NO | None | `seq >= 1` | None | No (with run_id) | D12. `UNIQUE(run_id, seq)`. |
| kind | Step kind | TEXT | NO | None | `kind IN ('model_turn', 'tool_call', 'approval')` | None | No | D12. |
| tool_name | Tool name (tool_call / approval) | TEXT | YES | NULL | `length(tool_name) > 0` | None | No | Agent roadmap (Task 4.2). |
| arguments | Raw JSON arguments exactly as provider-supplied | TEXT | YES | NULL | None | None | No | Agent roadmap (Task 4.2). |
| observation | Tool output / denial text / approval decision | TEXT | YES | NULL | None | None | No | Agent roadmap (Task 4.2). |
| status | Tool call outcome | TEXT | YES | NULL | `status IN ('succeeded', 'failed', 'denied', 'cancelled')` | None | No | Agent roadmap (Task 4.2). |
| started_at | Step start timestamp | INTEGER | NO | Current Unix timestamp | `started_at > 0` | None | No | Implementation Decision. |
| duration_ms | Step duration in milliseconds | INTEGER | YES | NULL | `duration_ms >= 0` | None | No | D12. |
| rule_id | Permission rule that decided this step (v7) | INTEGER | YES | NULL | None | permission_rules.id SET NULL | No | M1-core provenance. `NULL` for pre-v7 rows and non-gated steps. |
| group_key | Approval-group key batching related decisions (v7) | TEXT | YES | NULL | `group_key IS NULL OR length(group_key) <= 256` | None | No | M1-core provenance. |
| decided_by | Who/what decided this step (v7) | TEXT | YES | NULL | `decided_by IS NULL OR decided_by IN ('user', 'rule', 'system')` | None | No | M1-core provenance. |
| input_tokens | Provider-reported input tokens for the turn (v14) | INTEGER | YES | NULL | `input_tokens IS NULL OR input_tokens >= 0` | None | No | NEX-AGENT-005. `NULL` when the provider omitted usage. Saturating `u64 → i64` clamp on write. |
| output_tokens | Provider-reported output tokens for the turn (v14) | INTEGER | YES | NULL | `output_tokens IS NULL OR output_tokens >= 0` | None | No | NEX-AGENT-005. `NULL` when the provider omitted usage. |

---

### 7.10 permission_rules

**Purpose:** Ordered, revocable tool-permission rules (v7, M1-core): each row allows, gates (`ask`), or denies tool calls matching a preset + tool pattern + optional path pattern.
**Related FR:** None (agent roadmap, M1-core)

**CRUD:**
- **Create:** `INSERT` by user (or the v7 seed row: `coding` / `execute_command` / `*` / `ask` / priority 50).
- **Read:** `SELECT` all ordered by priority for rule evaluation.
- **Update:** None in MVP (delete + re-add).
- **Delete:** `DELETE` by `id`. `SET NULL` on `agent_steps.rule_id`.

**Deletion Behavior:** Hard delete. User-owned. `ON DELETE SET NULL` on `agent_steps.rule_id` — step history survives rule removal with provenance cleared.
**Rationale:** Rules are policy, steps are history; deleting policy must not destroy the audit trail.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| preset | Rule scope preset | TEXT | NO | None | `preset IN ('coding', 'document', '*')` | None | No (with tool_pattern, path_pattern) | M1-core. `UNIQUE(preset, tool_pattern, path_pattern)`. |
| tool_pattern | Tool name pattern | TEXT | NO | None | `length(tool_pattern) > 0 AND length(tool_pattern) <= 64` | None | No (with preset, path_pattern) | M1-core. |
| path_pattern | Optional path scope | TEXT | YES | NULL | `path_pattern IS NULL OR (length(path_pattern) > 0 AND length(path_pattern) <= 1024)` | None | No (with preset, tool_pattern) | M1-core. |
| effect | Rule outcome | TEXT | NO | None | `effect IN ('allow', 'ask', 'deny')` | None | No | M1-core. |
| priority | Evaluation order (lower first) | INTEGER | NO | 100 | `priority >= 0` | None | No | M1-core. |
| created_at | Creation timestamp | INTEGER | NO | Current Unix timestamp | `created_at > 0` | None | No | Implementation Decision. |

---

### 7.11 agent_tasks

**Purpose:** User-defined task lists with agent-executable steps (v8, task manager + autonomous mode): one row per task, driven by the bounded plan → act → verify → report loop.
**Related FR:** None (agent roadmap, task manager)

**CRUD:**
- **Create:** `INSERT` with `status = 'pending'`.
- **Read:** `SELECT` by `id`; `SELECT` by `status` ordered by `updated_at`.
- **Update:** `UPDATE` of title/description/status/progress/report/run link; `updated_at` maintained by explicit repository writes (no trigger).
- **Delete:** `DELETE` by `id`. Cascades to agent_task_steps and task_locks.

**Deletion Behavior:** Hard delete. User-owned. `ON DELETE CASCADE` to `agent_task_steps` and `task_locks`; also cascade-deleted when its conversation is removed (optional link, D50 privacy doctrine).
**Rationale:** A task and its steps/lock form a single unit of deletion; a conversation and its task history form a single unit of privacy.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| title | Task title | TEXT | NO | None | `length(title) > 0 AND length(title) <= 200` | None | No | Task manager. |
| description | Task detail | TEXT | YES | NULL | `description IS NULL OR length(description) <= 4000` | None | No | Task manager. |
| status | Task state | TEXT | NO | `'pending'` | `status IN ('pending', 'running', 'completed', 'failed', 'cancelled')` | None | No | Task manager. |
| conversation_id | Backing conversation | INTEGER | YES | NULL | `conversation_id IS NULL OR conversation_id > 0` | conversations.id CASCADE | No | Task manager; D50. |
| provider | Provider for the task loop | TEXT | YES | NULL | `provider IS NULL OR (length(provider) > 0 AND length(provider) <= 100)` | None | No | Task manager. |
| model | Model for the task loop | TEXT | YES | NULL | `model IS NULL OR (length(model) > 0 AND length(model) <= 200)` | None | No | Task manager. |
| max_steps | Step budget (capped) | INTEGER | NO | 25 | `max_steps >= 1 AND max_steps <= 25` | None | No | Task manager. Bounded autonomy. |
| current_step | Progress cursor | INTEGER | NO | 0 | `current_step >= 0` | None | No | Task manager. v13: `CHECK (current_step <= total_steps)`. |
| total_steps | Planned step count | INTEGER | NO | 0 | `total_steps >= 0` | None | No | Task manager. |
| report | Final verify/report text | TEXT | YES | NULL | None | None | No | Task manager. |
| run_id | Latest step's agent run | INTEGER | YES | NULL | `run_id IS NULL OR run_id > 0` | agent_runs.id SET NULL | No | Task manager. History survives run deletion. |
| created_at | Creation timestamp | INTEGER | NO | Current Unix timestamp | `created_at > 0` | None | No | Implementation Decision. |
| updated_at | Last modification timestamp | INTEGER | NO | Current Unix timestamp | `updated_at >= created_at` | None | No | Implementation Decision. |

---

### 7.12 agent_task_steps

**Purpose:** Agent-executable steps of a task (v8): one row per planned step, each optionally linked to its own agent run.
**Related FR:** None (agent roadmap, task manager)

**CRUD:**
- **Create:** `INSERT` (append) in step order.
- **Read:** `SELECT` by `task_id` ordered by `seq`.
- **Update:** `UPDATE` of status/result/run link/finish time during execution.
- **Delete:** Only via `ON DELETE CASCADE` from agent_tasks.

**Deletion Behavior:** Never deleted directly. `ON DELETE CASCADE` from agent_tasks.
**Rationale:** Step rows exist only while their task exists.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| task_id | Owning task | INTEGER | NO | None | `task_id > 0` | agent_tasks.id CASCADE | No (with seq) | Task manager. `UNIQUE(task_id, seq)`. |
| seq | 1-based step sequence within the task | INTEGER | NO | None | `seq >= 1` | None | No (with task_id) | Task manager. |
| title | Step title | TEXT | NO | None | `length(title) > 0 AND length(title) <= 500` | None | No | Task manager. |
| status | Step state | TEXT | NO | `'pending'` | `status IN ('pending', 'running', 'completed', 'failed', 'skipped', 'cancelled')` | None | No | Task manager. |
| result | Step outcome text | TEXT | YES | NULL | None | None | No | Task manager. |
| run_id | This step's agent run | INTEGER | YES | NULL | `run_id IS NULL OR run_id > 0` | agent_runs.id SET NULL | No | Task manager. History survives run deletion. |
| started_at | Step start timestamp | INTEGER | NO | Current Unix timestamp | `started_at > 0` | None | No | Implementation Decision. |
| finished_at | Step finish timestamp | INTEGER | YES | NULL | `finished_at IS NULL OR finished_at > 0` | None | No | Task manager. |

---

### 7.13 usage_ledger

**Purpose:** Local-only usage counts for the privacy center (v9): per-event-kind, per-day counters. Counts only — there is deliberately no text content column, so titles, prompts, message bodies, URLs, and error text can never be recorded here (enforced by the schema itself).
**Related FR:** None (privacy center)

**CRUD:**
- **Create/Update:** Upsert (`kind`, `day`) incrementing `count`.
- **Read:** `SELECT` by `day` range for `privacy_status` / `privacy_export`.
- **Delete:** Rows older than 90 days pruned by the recording path; full clear via user-confirmed `privacy_wipe` only.

**Deletion Behavior:** Pruned by age; wiped only on explicit user confirmation. No cascade.
**Rationale:** Nothing on this path ever leaves the machine.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| kind | Event kind | TEXT | NO | None | `kind IN ('agent_run', 'message', 'github_read', 'update_check')` | None | PK (with day) | Privacy center. Fixed vocabulary. |
| day | Day bucket | INTEGER | NO | None | `day > 0` | None | PK (with kind) | Privacy center. |
| count | Event count | INTEGER | NO | None | `count >= 0` | None | No | Privacy center. |

---

### 7.14 debt_items

**Purpose:** Technical-debt backlog (v10): one row per tracked debt item, manually added (`source='manual'`) or imported from the read-only repo-audit scanners (`source='audit'`).
**Related FR:** None (debt backlog)

**CRUD:**
- **Create:** `INSERT` manually or via audit import (`INSERT OR IGNORE` on `audit_key`).
- **Read:** `SELECT` by `status` ordered by `updated_at`.
- **Update:** `UPDATE` of status; `updated_at` refreshed explicitly on every status write (no trigger).
- **Delete:** `DELETE` by `id`.

**Deletion Behavior:** Hard delete. User-owned. No cascade.
**Rationale:** Backlog rows are standalone triage state.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| id | Surrogate primary key | INTEGER | NO | SQLite INTEGER PRIMARY KEY | `id > 0` | None | PK | Implementation Decision. |
| title | Debt title | TEXT | NO | None | `length(title) > 0 AND length(title) <= 200` | None | No | Debt backlog. |
| source | Row origin | TEXT | NO | None | `source IN ('audit', 'manual')` | None | No | Debt backlog. |
| severity | Severity | TEXT | NO | `'info'` | `severity IN ('info', 'warning')` | None | No | Debt backlog. Reuses the audit vocabulary. |
| status | Triage state | TEXT | NO | `'open'` | `status IN ('open', 'accepted', 'fixed', 'wontfix')` | None | No | Debt backlog. |
| location | Code location | TEXT | YES | NULL | `location IS NULL OR (length(location) > 0 AND length(location) <= 1024)` | None | No | Debt backlog. |
| note | Triage note | TEXT | YES | NULL | `note IS NULL OR length(note) <= 4000` | None | No | Debt backlog. |
| audit_key | Import identity | TEXT | YES | NULL | `audit_key IS NULL OR length(audit_key) <= 2048` | None | Yes | Debt backlog. `NULL` for manual rows (never collide — SQLite treats NULL UNIQUE members as distinct). |
| created_at | Creation timestamp | INTEGER | NO | Current Unix timestamp | `created_at > 0` | None | No | Implementation Decision. |
| updated_at | Last modification timestamp | INTEGER | NO | Current Unix timestamp | `updated_at >= created_at` | None | No | Implementation Decision. |

---

### 7.15 task_locks

**Purpose:** Kanban edit locks (v11): at most one lock row per task. `holder` is a short surface label, `token` is the opaque proof a mutating command must present, and `expires_at` is the mandatory deadline — a lock can never be eternal; an expired row is stealable by the next acquirer.
**Related FR:** None (kanban + locks)

**CRUD:**
- **Create:** `INSERT` on acquire (`task_id` PK ⇒ at most one lock per task).
- **Read:** `SELECT` by `task_id`; `SELECT` by `expires_at` for expiry.
- **Update:** `UPDATE` of token/expiry on heartbeat.
- **Delete:** `DELETE` on release/steal; `CASCADE` from task deletion.

**Deletion Behavior:** Hard delete on release. `ON DELETE CASCADE` from agent_tasks.
**Rationale:** A lock exists only while its task exists.

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|-------|---------|------|----------|---------|-------|-----|--------|--------------|
| task_id | Locked task (PK) | INTEGER | NO | None | `task_id > 0` | agent_tasks.id CASCADE | PK | Kanban + locks. |
| holder | Surface label | TEXT | NO | None | `length(holder) > 0 AND length(holder) <= 64` | None | No | Kanban + locks. |
| token | Opaque mutation proof | TEXT | NO | None | `length(token) > 0 AND length(token) <= 128` | None | Yes | Kanban + locks. |
| acquired_at | Acquisition timestamp | INTEGER | NO | None | `acquired_at > 0` | None | No | Kanban + locks. |
| expires_at | Mandatory deadline | INTEGER | NO | None | `expires_at > acquired_at` | None | No | Kanban + locks. |

---

## 8. INDEXES

| Indexed Columns | Table | Expected Query | Rationale | Traceability |
|-----------------|-------|----------------|-----------|--------------|
| `conversation_id`, `created_at` | messages | `SELECT * FROM messages WHERE conversation_id = ? ORDER BY created_at` | Primary history retrieval pattern. Eliminates sort operation. | FR-005 |
| `conversation_id` | attachments | `SELECT * FROM attachments WHERE conversation_id = ? AND message_id IS NULL` | Load draft attachments for a conversation. | FR-008 |
| `message_id` | attachments | `SELECT * FROM attachments WHERE message_id = ?` | Load historical attachments for a message. | FR-008 |
| `status`, `updated_at` | conversations | `SELECT * FROM conversations WHERE status = 'active' ORDER BY updated_at DESC` | List active conversations by recency. | FR-006 |
| `name` | providers | `SELECT * FROM providers WHERE name = ?` | Resolve provider by internal name for keyring lookup. | FR-004, FR-014 |
| `run_id`, `seq` | agent_steps | `SELECT * FROM agent_steps WHERE run_id = ? ORDER BY seq` | Ordered step retrieval for a run. | Agent roadmap (Task 4.2). |
| `conversation_id` | agent_runs | `SELECT * FROM agent_runs WHERE conversation_id = ? ORDER BY started_at DESC` | List the agent runs of one conversation (M5). | Agent roadmap (Task 4.2). |
| `started_at` | agent_runs | `SELECT * FROM agent_runs ORDER BY started_at DESC` | Run history listing by recency. | Agent roadmap (Task 4.2). |
| `conversation_id`, `message_id` | attachments | `SELECT * FROM attachments WHERE conversation_id = ? AND message_id = ?` | Same-conversation link lookup backing the v13 composite FK. | NEX-DB-002 (v13). |
| `status`, `updated_at` | agent_tasks | `SELECT * FROM agent_tasks WHERE status = ? ORDER BY updated_at DESC` | Task listing by triage state and recency. | Task manager (v8). |
| `task_id`, `seq` | agent_task_steps | `SELECT * FROM agent_task_steps WHERE task_id = ? ORDER BY seq` | Ordered step retrieval for a task. | Task manager (v8). |
| `day` | usage_ledger | `SELECT * FROM usage_ledger WHERE day BETWEEN ? AND ?` | Privacy-center range reads over day buckets. | Privacy center (v9). |
| `status`, `updated_at` | debt_items | `SELECT * FROM debt_items WHERE status = ? ORDER BY updated_at DESC` | Backlog listing by triage state and recency. | Debt backlog (v10). |
| `expires_at` | task_locks | `SELECT * FROM task_locks WHERE expires_at <= ?` | Expiry scans for stealable locks. | Kanban + locks (v11). |

UNIQUE/PK-backed implicit indexes (SQLite `sqlite_autoindex_*`, not created explicitly): `providers.name`, `messages(conversation_id, id)` (v13), `agent_steps(run_id, seq)`, `permission_rules(preset, tool_pattern, path_pattern)` (v7), `agent_task_steps(task_id, seq)` (v8), `usage_ledger(kind, day)` PK (v9), `debt_items.audit_key` (v10), `task_locks.token` (v11).

---

## 9. FOREIGN KEYS

| Child Table | Column | Parent Table | Parent Column | On Delete | Rationale |
|-------------|--------|--------------|---------------|-----------|-----------|
| messages | conversation_id | conversations | id | CASCADE | Message history is part of the conversation. FR-013 requires that conversation removal delete its data. |
| messages | provider_id | providers | id | SET NULL | Provider removal (FR-014) must not destroy message history (FR-005). The provider reference is cleared. |
| attachments | conversation_id | conversations | id | CASCADE | Attachments belong to the conversation context. Removed with the conversation per FR-013. |
| attachments | message_id | messages | id | CASCADE | When a message is deleted, its linked attachments are removed. Draft attachments (`message_id IS NULL`) are unaffected by message deletion, but are removed via `conversation_id` CASCADE when the conversation is deleted. |
| agent_runs | conversation_id | conversations | id | CASCADE | Optional link: a run may not yet be tied to a conversation (NULL until Task 5.1). Removing a conversation atomically removes its agent runs (Nexora privacy doctrine, D50). |
| agent_steps | run_id | agent_runs | id | CASCADE | Step records are part of the run (D12). Removing a run atomically removes its steps. |
| agent_steps | rule_id | permission_rules | id | SET NULL | Optional provenance (v7): rule removal clears the decision link but preserves step history. |
| agent_tasks | conversation_id | conversations | id | CASCADE | Optional link (v8): removing a conversation atomically removes its tasks (D50). |
| agent_tasks | run_id | agent_runs | id | SET NULL | Latest-step run link (v8): run deletion clears the link but preserves the task. |
| agent_task_steps | task_id | agent_tasks | id | CASCADE | Step rows are part of the task (v8). |
| agent_task_steps | run_id | agent_runs | id | SET NULL | Own-step run link (v8): run deletion clears the link but preserves the step. |
| task_locks | task_id | agent_tasks | id | CASCADE | A lock exists only while its task exists (v11). |
| attachments | (conversation_id, message_id) | messages | (conversation_id, id) | CASCADE | Composite same-conversation guard (v13): an attachment can only link a message of its own conversation. Coexists with the single-column FKs; draft rows (`message_id IS NULL`) skip it under SQLite NULL semantics. |

---

## 10. FULL TEXT SEARCH

**Indexed Content:**
- `conversations_fts`: indexes `conversations.title` — FR-006 (conversation search), FR-009.
- `messages_fts`: indexes `messages.content` — FR-009 (conversation content search).  
  **Implementation Decision:** Searching only conversation titles would fail to locate topics discussed within messages. Indexing message content is necessary to satisfy FR-009.
- `prompts_fts`: indexes `prompts.title` and `prompts.content` — FR-007, FR-009.

**Synchronization:** Database triggers on `INSERT`, `UPDATE`, and `DELETE` maintain FTS virtual tables.

**Tokenizer:** Tokenizer selection is an implementation decision and is not specified at the architectural level.

---

## 11. TRIGGERS

**FTS Synchronization Triggers:**
- **Purpose:** Maintain FTS5 virtual tables.
- **Timing:** AFTER INSERT, AFTER UPDATE, AFTER DELETE.
- **Tables:** `conversations`, `messages`, `prompts`.
- **Rationale:** FTS5 virtual tables do not auto-update. Triggers are required to keep search results current for FR-009.

**Updated-At Triggers:**
- **Purpose:** Automatically update `updated_at` timestamps when user-editable fields change.
- **Timing:** AFTER UPDATE.
- **Tables:** `conversations`, `prompts`.
- **Fields:** For `conversations`, the trigger executes only when `title` or `status` changes. For `prompts`, the trigger executes only when `title` or `content` changes.
- **Rationale:** Ensures timestamp accuracy without requiring application-layer logic to manually set the field on every update. Not applied to `messages` (immutable) or `attachments` (only `message_id` is updated once at send time, which does not represent a semantic modification requiring timestamp tracking).
- **v12 monotonicity (NEX-DB-001/003):** the trigger bodies stamp `max(unixepoch(), created_at)` instead of bare `unixepoch()`, so rows with a future `created_at` (within-skew import, backward clock movement) no longer fail their next update against the `updated_at >= created_at` CHECK. Drop + recreate; no row data touched.

---

## 12. TRANSACTIONS

**Conversation Creation:** `INSERT` into `conversations`. Atomic to prevent partial creation.

**Message Send:** `INSERT` user `message`; `UPDATE` `attachments.message_id` for linked draft attachments; `UPDATE` `conversations.updated_at`. Atomic to ensure that a message and its linked attachments are committed together.

**Conversation Delete:** `DELETE` from `conversations` by `id`. Single statement; `CASCADE` handles dependent messages and attachments atomically per FR-013.

**Prompt CRUD:** Single-row `INSERT`, `UPDATE`, or `DELETE` operations. Wrapped in transactions for consistency.

**Import:** All `INSERT`s for imported conversations, messages, and prompts within one transaction. Rolls back entirely on validation failure per FR-011.

**Export:** Read-only `SELECT` statements. A read transaction ensures snapshot consistency per FR-010.

---

## 13. DATA INTEGRITY

**CHECK Constraints:** Enum-like columns use `CHECK` to prevent invalid values at the database level: `conversations.status`, `messages.role`, `agent_runs.status`/`mode`, `agent_steps.kind`/`status`/`decided_by`, `permission_rules.preset`/`effect`, `agent_tasks.status`, `agent_task_steps.status`, `usage_ledger.kind`, `debt_items.source`/`severity`/`status`, plus numeric bounds (length caps, non-negative counters, `max_steps` 1–25, `expires_at > acquired_at`, token counts ≥ 0) and the v13 cross-column invariants (`agent_tasks.current_step <= total_steps`; terminal `agent_runs` require `finished_at`; `finished_at >= started_at`).

**UNIQUE Constraints:** `providers.name` prevents duplicate provider configurations. `app_settings.key` is the primary key, preventing duplicate settings. `agent_steps(run_id, seq)` and `agent_task_steps(task_id, seq)` enforce step ordering integrity. `permission_rules(preset, tool_pattern, path_pattern)` prevents duplicate rules. `debt_items.audit_key` deduplicates audit imports. `task_locks.token` keeps mutation proofs unguessable-per-row. `messages(conversation_id, id)` (v13) anchors the attachment composite FK.

**Referential Integrity:** Foreign keys ensure that every message belongs to a valid conversation, every attachment belongs to a valid conversation, and every message provider reference is valid or `NULL`. Agent history follows the same discipline: steps belong to a run, tasks optionally belong to a conversation, task steps belong to a task, locks belong to a task, and run links from tasks clear (`SET NULL`) instead of destroying task history.

**Hard Delete:** All user-managed entities use hard delete per FR-013. `CASCADE` ensures no orphaned data. No soft delete is implemented; SRS explicitly requires that users can permanently delete data.

---

## 14. SECURITY

**Stored in SQLite:**
- Conversation titles and message content
- Prompt library content
- Application settings
- Provider non-sensitive metadata (`name`, `display_name`)
- Attachment metadata (file paths, names, sizes, types)
- Agent run/step history (`agent_runs`, `agent_steps`, including spend caps, token counts, and rule provenance — never credentials, never reasoning signatures)
- Permission rules, agent tasks and task steps, kanban task locks
- Local-only usage counts (`usage_ledger`: fixed kinds + day buckets + counters, no content)
- Debt backlog items

**Never Stored in SQLite:**
- API keys
- Provider secrets
- Authentication tokens
- Passwords

**Requirement:** Provider credentials MUST NEVER be stored in SQLite. They MUST ONLY be stored in the operating system secure keyring. The `providers.name` field is used by the application layer to derive the keyring entry identifier.  
**Reference:** FR-014, ARCHITECTURE.md Section 12.

---

## 15. BACKUP

**Philosophy:** The database is a single self-contained file. Backup is performed by filesystem copy.

**Restore:** Replacement of the database file. On launch, the schema version is verified. If the backup file is incompatible with the application version, the application refuses to start and alerts the user.

**Integrity:** On detection of abnormal termination, `PRAGMA integrity_check` may be run before write operations.

**Corruption:** If the database file is corrupted and cannot be opened, the application must refuse to use the corrupted database and notify the user. Creation of a new empty database with the current schema may occur only after explicit user confirmation.

---

## 16. IMPORT / EXPORT

**Export (FR-010):** Reads a conversation and its messages ordered by `created_at`. Output preserves message sequence, role, content, provider reference, and model name.

**Import (FR-011):** Validates required fields before insertion. Imported conversations receive new surrogate identifiers to avoid primary key conflicts. All inserts are atomic within a single transaction; validation failures roll back entirely.

**Conflicts:** Import does not merge with existing data. All imported items are inserted as new rows.

---

## 17. FUTURE EXTENSION POINTS

The schema supports future evolution through:

- **Schema versioning:** Additive migrations can introduce new tables or columns without breaking existing data.
- **App settings key-value structure:** New user preferences can be added without modifying the `app_settings` table schema.

No tables, columns, or indexes are reserved for speculative future features.