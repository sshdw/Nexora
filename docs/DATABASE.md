 # Nexora Database Architecture Specification

## 1. Purpose

This document defines the complete SQLite database architecture for the Nexora MVP. The database is the single source of truth for all local persistent state, responsible for storing conversations, messages, prompts, attachments, provider metadata, and application settings. It enables offline-first operation, local full-text search, and data portability as required by SRS.md.

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
JSON is not used in any column.  
**Rationale:** All MVP requirements are satisfied with a strictly relational schema. Introducing JSON for provider configuration (as considered in the previous draft) was unnecessary; provider-specific non-sensitive data does not require schema-flexible storage in the MVP.

### FTS5 Usage
FTS5 virtual tables are used for full-text search.  
**Rationale:** Required by FR-009 for offline local search across conversations and prompts. FTS5 provides efficient tokenization and ranking without external dependencies.

---

## 4. Schema Versioning

Schema changes are tracked in the `schema_version` table. Each migration inserts exactly one new row containing a monotonically increasing version number and an application timestamp. Version numbers are unique and strictly increasing. The current schema version is determined by the maximum version number present (`MAX(version)`), which therefore always represents the current schema version.

**Rationale:** This provides an unambiguous, ordered migration path and prevents the application from running against an unrecognized schema version.

---

## 5. Migration Strategy

Migrations follow a forward-only, incremental philosophy. Each migration is atomic and versioned. The application refuses to start if the database file's schema version is newer than the application's known migration set, preventing backward incompatibility. Migrations are executed within a transaction; if any step fails, the entire migration rolls back, preserving database integrity. No destructive data changes occur without explicit validation.

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

---

## 8. INDEXES

| Indexed Columns | Table | Expected Query | Rationale | Traceability |
|-----------------|-------|----------------|-----------|--------------|
| `conversation_id`, `created_at` | messages | `SELECT * FROM messages WHERE conversation_id = ? ORDER BY created_at` | Primary history retrieval pattern. Eliminates sort operation. | FR-005 |
| `conversation_id` | attachments | `SELECT * FROM attachments WHERE conversation_id = ? AND message_id IS NULL` | Load draft attachments for a conversation. | FR-008 |
| `message_id` | attachments | `SELECT * FROM attachments WHERE message_id = ?` | Load historical attachments for a message. | FR-008 |
| `status`, `updated_at` | conversations | `SELECT * FROM conversations WHERE status = 'active' ORDER BY updated_at DESC` | List active conversations by recency. | FR-006 |
| `name` | providers | `SELECT * FROM providers WHERE name = ?` | Resolve provider by internal name for keyring lookup. | FR-004, FR-014 |

---

## 9. FOREIGN KEYS

| Child Table | Column | Parent Table | Parent Column | On Delete | Rationale |
|-------------|--------|--------------|---------------|-----------|-----------|
| messages | conversation_id | conversations | id | CASCADE | Message history is part of the conversation. FR-013 requires that conversation removal delete its data. |
| messages | provider_id | providers | id | SET NULL | Provider removal (FR-014) must not destroy message history (FR-005). The provider reference is cleared. |
| attachments | conversation_id | conversations | id | CASCADE | Attachments belong to the conversation context. Removed with the conversation per FR-013. |
| attachments | message_id | messages | id | CASCADE | When a message is deleted, its linked attachments are removed. Draft attachments (`message_id IS NULL`) are unaffected by message deletion, but are removed via `conversation_id` CASCADE when the conversation is deleted. |

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

**CHECK Constraints:** Enum-like columns use `CHECK` to prevent invalid values at the database level: `conversations.status`, `messages.role`.

**UNIQUE Constraints:** `providers.name` prevents duplicate provider configurations. `app_settings.key` is the primary key, preventing duplicate settings.

**Referential Integrity:** Foreign keys ensure that every message belongs to a valid conversation, every attachment belongs to a valid conversation, and every message provider reference is valid or `NULL`.

**Hard Delete:** All user-managed entities use hard delete per FR-013. `CASCADE` ensures no orphaned data. No soft delete is implemented; SRS explicitly requires that users can permanently delete data.

---

## 14. SECURITY

**Stored in SQLite:**
- Conversation titles and message content
- Prompt library content
- Application settings
- Provider non-sensitive metadata (`name`, `display_name`)
- Attachment metadata (file paths, names, sizes, types)

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