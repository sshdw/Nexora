//! Tauri IPC layer.
//!
//! Thin, typed wrappers around the existing Phase 10.2 `#[tauri::command]`
//! functions. No business logic lives here — commands are delegated to the
//! backend verbatim. Field names mirror the Rust structs serialized via serde
//! (snake_case). Timestamps are Unix seconds (SQLite `unixepoch()`), per
//! DATABASE.md §7.1.

import { invoke } from "@tauri-apps/api/core";

/** A conversation row as persisted by the backend (DATABASE.md §7.1). */
export interface Conversation {
  id: number;
  title: string;
  status: "active" | "archived";
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
  /** Canonical workspace root the conversation belongs to (v6 per-folder
   * history; `null` for pre-picker rows). */
  workspace_root: string | null;
}

/** Safe, secret-free command error returned to the frontend (commands/error.rs). */
export interface CommandError {
  kind: string;
  message: string;
}

/** List all conversations via the `list_conversations` command.
 * The backend owns ordering (`updated_at DESC`); the frontend does not re-sort. */
export function listConversations(): Promise<Conversation[]> {
  return invoke<Conversation[]>("list_conversations");
}

/** Create a conversation via the `create_conversation` command.
 * The backend requires a non-empty title (<=500 chars per the schema CHECK). */
export function createConversation(title: string): Promise<number> {
  return invoke<number>("create_conversation", { title });
}

/** One persisted `messages` row (DATABASE.md §7.2). */
export interface Message {
  id: number;
  conversation_id: number;
  role: "user" | "assistant";
  content: string;
  provider_id: number | null;
  model_name: string | null;
  created_at: number; // seconds since unix epoch
}

/** Load a conversation's persisted messages via `conversation_history`, in the
 * backend's chronological order (`created_at` ascending). The backend is the
 * only source of history; the UI never invents or reorders messages. */
export function conversationHistory(conversationId: number): Promise<Message[]> {
  return invoke<Message[]>("conversation_history", { conversationId });
}

/** Rename a conversation via `rename_conversation` (FR-002, FR-006). */
export function renameConversation(id: number, title: string): Promise<void> {
  return invoke<void>("rename_conversation", { id, title });
}

/** Archive an active conversation via `archive_conversation` (FR-006). */
export function archiveConversation(id: number): Promise<void> {
  return invoke<void>("archive_conversation", { id });
}

/** Restore an archived conversation to active via `restore_conversation` (FR-006). */
export function restoreConversation(id: number): Promise<void> {
  return invoke<void>("restore_conversation", { id });
}

/** Delete a conversation and cascade its messages/attachments (FR-002). */
export function deleteConversation(id: number): Promise<void> {
  return invoke<void>("delete_conversation", { id });
}

// ---- Providers -------------------------------------------------------
// Provider metadata (non-sensitive) and supported-provider/model definitions.

/** A configured provider row (`providers` table, DATABASE.md §7.5). */
export interface ProviderDef {
  id: number;
  name: string;
  display_name: string;
}

/** A build-supported provider with its hardcoded supported models
 * (DATABASE.md §7.5). Exposed by the backend so the UI never invents
 * providers or models. */
export interface SupportedProvider {
  name: string;
  display_name: string;
  models: string[];
}

/** List all configured providers (metadata only) via `list_providers`. */
export function listProviders(): Promise<ProviderDef[]> {
  return invoke<ProviderDef[]>("list_providers");
}

/** List the providers supported by this build, with their models. */
export function supportedProviders(): Promise<SupportedProvider[]> {
  return invoke<SupportedProvider[]>("supported_providers");
}

/** Register a new provider definition (FR-004). Backend rejects duplicates. */
export function createProvider(name: string, displayName: string): Promise<number> {
  return invoke<number>("create_provider", { name, displayName });
}

/** Remove a provider definition by id (FR-004 / provider configuration). */
export function removeProvider(id: number): Promise<void> {
  return invoke<void>("remove_provider", { id });
}

/** Report whether a provider is configured and has stored credentials. */
export function isProviderAvailable(name: string): Promise<boolean> {
  return invoke<boolean>("is_provider_available", { name });
}

/** Local health verdict for one provider (`provider_health`, WS-A.5).
 * Local-only: no network probe — `healthy` means locally ready to serve. */
export type ProviderHealthStatus = "healthy" | "degraded" | "unreachable" | "unknown";

/** Local health snapshot for one provider. Metadata only — presence booleans
 * plus a `last_checked` Unix-seconds timestamp; never a credential value.
 * `last_checked` is `0` only when the backend clock was unavailable: render
 * it as "unknown", never as the epoch. */
export interface ProviderHealth {
  provider: string;
  status: ProviderHealthStatus;
  has_configuration: boolean;
  has_credential: boolean;
  last_checked: number; // seconds since unix epoch
}

/** Probe the local health of one provider via `provider_health`. */
export function providerHealth(name: string): Promise<ProviderHealth> {
  return invoke<ProviderHealth>("provider_health", { name });
}

// ---- OpenAI-compatible endpoint (custom base URL) ----------------------
// The endpoint metadata (base URL, model, organization, headers) is plain
// settings; the API key stays in the OS keyring under `openai_compat` and
// only its presence is ever exposed.

/** User configuration for the generic OpenAI-compatible endpoint
 * (`openai_compat`). Field names mirror the Rust struct (snake_case).
 * Carries no secret — the key is keyring-only. */
export interface CompatConfig {
  base_url: string;
  model: string;
  organization: string | null;
  headers: [string, string][];
  supports_tools: boolean;
}

/** UI-facing status of the OpenAI-compatible endpoint: presence booleans
 * plus readiness. Carries metadata only — never a secret value. */
export interface CompatStatus {
  has_base_url: boolean;
  base_url_valid: boolean;
  has_model: boolean;
  has_organization: boolean;
  header_count: number;
  has_credential: boolean;
  ready: boolean;
}

/** Read the stored OpenAI-compatible endpoint configuration. */
export function getCompatConfig(): Promise<CompatConfig> {
  return invoke<CompatConfig>("get_compat_config");
}

/** Persist the OpenAI-compatible endpoint configuration. The backend
 * validates it and rejects it unchanged on failure. */
export function setCompatConfig(config: CompatConfig): Promise<void> {
  return invoke<void>("set_compat_config", { config });
}

/** Report the UI-facing status (presence booleans + readiness) of the
 * OpenAI-compatible endpoint. */
export function compatStatus(): Promise<CompatStatus> {
  return invoke<CompatStatus>("compat_status");
}

// ---- Credentials -----------------------------------------------------
// Values stay in the OS secure keyring; only presence is ever exposed.

/** Whether `provider` has a stored keyring credential (never the value). */
export function hasProviderCredential(provider: string): Promise<boolean> {
  return invoke<boolean>("has_provider_credential", { provider });
}

/** Store a new keyring credential for `provider` (FR-014). */
export function addProviderCredential(provider: string, credential: string): Promise<void> {
  return invoke<void>("add_provider_credential", { provider, credential });
}

/** Update the stored keyring credential for `provider` (FR-014). */
export function updateProviderCredential(provider: string, credential: string): Promise<void> {
  return invoke<void>("update_provider_credential", { provider, credential });
}

/** Remove the stored keyring credential for `provider` (no-op if absent). */
export function removeProviderCredential(provider: string): Promise<void> {
  return invoke<void>("remove_provider_credential", { provider });
}

// ---- Settings (FR-012) ----------------------------------------------

/** Read one setting by key (`null` when absent), via `get_setting`. */
export function getSetting(key: string): Promise<string | null> {
  return invoke<string | null>("get_setting", { key });
}

/** Write one setting by key (value may be `null`), via `set_setting`. */
export function setSetting(key: string, value: string | null): Promise<void> {
  return invoke<void>("set_setting", { key, value });
}

/** Delete one setting by key, via `delete_setting`. */
export function deleteSetting(key: string): Promise<void> {
  return invoke<void>("delete_setting", { key });
}

// ---- Data management (FR-013) ------------------------------------------

// Server-side destructive-action confirmations (NEX-SEC-004): the backend
// raises a BLOCKING NATIVE OS CONFIRMATION DIALOG and only mints a
// single-use id once the user accepts, then the destructive command consumes
// that id exactly once. A bare IPC caller can therefore no longer obtain an
// id unattended, and the id is bound to the exact operation it was minted
// for — the old caller-supplied phrase/boolean gates are gone.
//
// Consequence for callers: there is NO in-app confirm step for these paths.
// The native dialog is the single, honest confirmation. Rejections mean the
// user cancelled at the native dialog and NOTHING ran.

/** Scope a minted confirmation id is bound to. Ids never cross scopes. */
export type ConfirmationScope = "terminal" | "data_management";

/** Operation identities for the row-scoped / clear-all data-management
 * paths. These literals must match the Rust `OP_*` constants
 * (`application/confirmations.rs`) exactly — the backend binds the id to them
 * and recomputes that binding on consume, so a drift refuses every call. A
 * source check in `commands/confirmations.rs` pins them. */
const OP_DELETE_PROMPT = "delete_prompt";
const OP_DELETE_CONVERSATION = "delete_conversation";
const OP_CLEAR_ALL_DATA = "clear_application_data";

/** Request one single-use confirmation id for `operation` via
 * `request_confirmation`.
 *
 * This shows a native OS confirmation dialog and only resolves once the user
 * accepts; it REJECTS (with `kind: "confirmationRequired"`) on cancel, and
 * nothing is minted or executed in that case. The operation identity is what
 * the user is shown and what the id is bound to — never a free-form summary
 * the renderer could decouple from what actually runs.
 *
 * @param scope Confirmation scope the id is bound to.
 * @param operation Operation identity (command text, or an `OP_*` literal).
 * @param target Optional extra binding: workspace-relative `cwd` for terminal
 * runs and the affected row id for row-scoped deletes.
 */
export function requestConfirmation(
  scope: ConfirmationScope,
  operation: string,
  target?: { cwd?: string | null; targetId?: number | null },
): Promise<string> {
  return invoke<string>("request_confirmation", {
    scope,
    operation,
    cwd: target?.cwd ?? null,
    targetId: target?.targetId ?? null,
  });
}

/** Clear ALL local application data (conversations, messages, attachments,
 * prompts, provider metadata, settings) via the existing Phase 9
 * `clear_application_data` command.
 *
 * The native OS confirmation dialog this raises IS the confirmation — there is
 * no separate in-app confirm step, and the id it mints is bound to clear-all
 * specifically, so an id the user approved for a single-row delete cannot be
 * spent here. Cancelling at the dialog rejects and deletes nothing.
 * Keyring credentials are not touched (they never lived in SQLite). */
export async function clearApplicationData(): Promise<void> {
  const confirmationId = await requestConfirmation(
    "data_management",
    OP_CLEAR_ALL_DATA,
  );
  return invoke<void>("clear_application_data", { confirmationId });
}

// ---- Search (FR-006, FR-009) -------------------------------------------

/** One `prompts` row as persisted (DATABASE.md §7.3). */
export interface Prompt {
  id: number;
  title: string;
  content: string;
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
}

/** Grouped results of one `search` call (BACKEND application/search.rs). */
export interface SearchResults {
  /** Conversations whose title matched, ordered by relevance. */
  conversations: Conversation[];
  /** Messages whose content matched; each opens its `conversation_id`. */
  message_matches: Message[];
  /** Prompts whose title/content matched. */
  prompts: Prompt[];
}

/** Run the existing local `search` command over conversations, messages, and
 * prompts. A blank query yields empty results (backend contract). */
export function search(query: string): Promise<SearchResults> {
  return invoke<SearchResults>("search", { query });
}

// ---- Prompt Library (FR-007) ---------------------------------------------

/** List every saved prompt via `list_prompts`. The backend returns rows in
 * creation order; the Prompt Library screen sorts by `updated_at` locally. */
export function listPrompts(): Promise<Prompt[]> {
  return invoke<Prompt[]>("list_prompts");
}

/** Create a prompt via `create_prompt` and return its schema-assigned id
 * (FR-007; DATABASE.md §7.3). */
export function createPrompt(title: string, content: string): Promise<number> {
  return invoke<number>("create_prompt", { title, content });
}

/** Update a prompt's `title` / `content` via `update_prompt` (FR-007). */
export function updatePrompt(id: number, title: string, content: string): Promise<void> {
  return invoke<void>("update_prompt", { id, title, content });
}

/** Permanently delete one prompt via `delete_prompt_permanently`.
 *
 * The native OS confirmation dialog this raises IS the confirmation — the
 * caller does NOT show its own confirm dialog, and the id it mints is bound
 * to this exact row, so an id the user approved for another prompt (or for
 * clear-all) is refused. Cancelling at the native dialog rejects and deletes
 * nothing (FR-007). */
export async function deletePrompt(id: number): Promise<void> {
  const confirmationId = await requestConfirmation("data_management", OP_DELETE_PROMPT, {
    targetId: id,
  });
  return invoke<void>("delete_prompt_permanently", {
    id,
    confirmationId,
  });
}

/** Permanently delete one conversation via `delete_conversation_permanently`
 * (the data-management path, as distinct from the softer
 * {@link deleteConversation} above). Binds the confirmation to this exact row
 * so an id approved for another conversation cannot delete it. */
export async function deleteConversationPermanently(id: number): Promise<void> {
  const confirmationId = await requestConfirmation(
    "data_management",
    OP_DELETE_CONVERSATION,
    { targetId: id },
  );
  return invoke<void>("delete_conversation_permanently", {
    id,
    confirmationId,
  });
}

// ---- Import / Export (FR-010, FR-011) ---------------------------------
// Thin wrappers over the existing Phase 8 application-layer services,
// already exposed as Tauri commands (commands/import_export.rs).

/** Export one conversation to the JSON document at `path` via
 * `export_conversation_to_file`. Read-only against the database; the
 * document preserves persisted message order (FR-010). */
export function exportConversationToFile(id: number, path: string): Promise<void> {
  return invoke<void>("export_conversation_to_file", { id, path });
}

/** Import one conversation from an exported JSON document string via
 * `import_conversation`; returns the schema-assigned id of the newly created
 * conversation (FR-011). The backend inserts atomically — validation failure
 * leaves no partial rows behind. */
export function importConversation(json: string): Promise<number> {
  return invoke<number>("import_conversation", { json });
}

// ---- Setup import/export (WS-E.2) --------------------------------------
// VS Code settings + MCP servers import and the portable Nexora setup
// document. Reports echo caller-supplied key names only (never values);
// denials carry fixed-vocabulary reasons.

/** One successfully translated key: source key plus the Nexora key written. */
export interface ImportedEntry {
  source_key: string;
  nexora_key: string;
}

/** One rejected key: source key plus a fixed-vocabulary reason. */
export interface DeniedEntry {
  source_key: string;
  reason: string;
}

/** Per-key outcome of a setup import: translated, skipped (no Nexora
 * counterpart, never guessed), and denied (fixed-vocabulary reason). */
export interface SetupImportReport {
  imported: ImportedEntry[];
  skipped: string[];
  denied: DeniedEntry[];
}

/** Import a VS Code `settings.json` document; returns the per-key report.
 * Only the mappable subset translates (`workbench.colorTheme`); everything
 * else is reported as skipped. */
export function importVscodeSettings(json: string): Promise<SetupImportReport> {
  return invoke<SetupImportReport>("import_vscode_settings", { json });
}

/** Import an MCP servers document (`{ "mcpServers": { ... } }`); returns the
 * per-server report. Validated servers replace the stored `mcp.servers` list. */
export function importMcpServers(json: string): Promise<SetupImportReport> {
  return invoke<SetupImportReport>("import_mcp_servers", { json });
}

/** Export the current Nexora setup (settings plus routing profiles and
 * feature flags) to its portable JSON document. */
export function exportSetup(): Promise<string> {
  return invoke<string>("export_setup");
}

/** Export the current Nexora setup to the JSON document at `path`. */
export function exportSetupToFile(path: string): Promise<void> {
  return invoke<void>("export_setup_to_file", { path });
}

/** Import a portable Nexora setup document; returns the per-key report. */
export function importSetup(json: string): Promise<SetupImportReport> {
  return invoke<SetupImportReport>("import_setup", { json });
}

// ---- AI execution ----------------------------------------------------

/** One persisted `attachments` row (DATABASE.md §7.4). A draft attachment has
 * `message_id: null`; a sent attachment carries the user message it belongs
 * to. The absolute `file_path` is backend bookkeeping and is never rendered. */
export interface Attachment {
  id: number;
  conversation_id: number;
  message_id: number | null;
  file_name: string;
  file_path: string;
  file_size_bytes: number | null;
  mime_type: string | null;
}

/** Attach a local-file reference to the conversation as a draft attachment via
 * the existing `attach_file` command (FR-008). No content is uploaded; only
 * metadata (name, path, size, media type) is persisted locally. */
export function attachFile(
  conversationId: number,
  fileName: string,
  filePath: string,
  fileSizeBytes: number | null,
  mimeType: string | null,
): Promise<Attachment> {
  return invoke<Attachment>("attach_file", {
    conversationId,
    fileName,
    filePath,
    fileSizeBytes,
    mimeType,
  });
}

/** List the conversation's draft attachments (`message_id` IS NULL) via
 * `list_attachments`. Historical, message-linked rows are not included. */
export function listAttachments(conversationId: number): Promise<Attachment[]> {
  return invoke<Attachment[]>("list_attachments", { conversationId });
}

/** Hard-delete one draft attachment via `remove_attachment`. Other rows,
 * messages, and the conversation are untouched. */
export function removeAttachment(id: number): Promise<void> {
  return invoke<void>("remove_attachment", { id });
}

export interface ToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
}

/** Normalized AI response returned by `send_message`. */
export interface AiResponse {
  content: string;
  model: string;
  tool_calls: ToolCall[];
  usage?: TokenUsage | null;
}

/** Send a user message and return/ persist the AI response.
 * `attachmentIds` names the draft attachments to link to the created user
 * message; they become part of the AI request context (FR-008).
 * The backend resolves the provider row and keyring credential before any
 * outbound request is made, so missing credentials fail before sending. */
export function sendMessage(
  conversationId: number,
  content: string,
  provider: string,
  model: string,
  attachmentIds: number[],
): Promise<AiResponse> {
  return invoke<AiResponse>("send_message", {
    conversationId,
    content,
    provider,
    model,
    attachmentIds,
  });
}

// ---- Agent runs (Task 5.1) ------------------------------------------

/** One `agent_runs` row as persisted by the backend (DATABASE.md §7.8). */
export interface AgentRun {
  id: number;
  conversation_id: number | null;
  model: string;
  mode: string;
  status: string;
  started_at: number;
  finished_at: number | null;
  total_steps: number;
  final_content: string | null;
  error: string | null;
  spent_micro_usd: number | null;
  limit_micro_usd: number | null;
}

/** One `agent_steps` row as persisted (DATABASE.md §7.9). */
export interface AgentStep {
  id: number;
  run_id: number;
  seq: number;
  kind: string;
  tool_name: string | null;
  arguments: string | null;
  observation: string | null;
  status: string | null;
  started_at: number;
  duration_ms: number | null;
  rule_id: number | null;
  group_key: string | null;
  decided_by: string | null;
}

/** Step payload of a `RunFrame::Step` frame (Task 5.1). */
export interface StepEventFrame {
  seq: number;
  kind: string;
  tool_name: string | null;
  arguments: string | null;
  observation: string | null;
  status: string | null;
  duration_ms: number | null;
}

/** Terminal payload of a `RunFrame::Finished` frame (Task 5.1). */
export interface RunFinishedPayload {
  conversation_id: number;
  status: string;
  final_content: string | null;
  error: string | null;
}

/** Governance event payload streamed inside a `RunFrame::Governance`. */
export type GovernanceEventPayload =
  | { type: "paused" }
  | { type: "resumed" }
  | { type: "budget_exhausted"; max_steps: number }
  | { type: "spend_limit_exceeded"; spent_micro: number; limit_micro: number }
  | { type: "approval_requested"; call_id: string; name: string; arguments: string; group_key: string | null; group_size: number }
  | { type: "approval_resolved"; call_id: string; approved: boolean }
  | { type: "cancelled" }
  | { type: "completed"; steps: number }
  | { type: "compaction_started"; reason: string; messages: number }
  | { type: "compaction_finished"; reason: string; summarized: number; retained: number }
  | { type: "compaction_failed"; reason: string };

/** One `agent-run-event` frame (Task 5.1 design §2.4). */
export type AgentRunEventPayload =
  | { type: "step"; run_id: number; event: StepEventFrame }
  | { type: "governance"; run_id: number; event: GovernanceEventPayload }
  | { type: "finished"; run_id: number; event: RunFinishedPayload };

// NOTE: command ARGS are camelCase (Tauri v2); payloads/events stay snake_case
// (serde `rename_all = "snake_case"`) — never align one to the other.

/** Start one opt-in agent run for `conversationId` (Task 5.1).
 * Returns `{ run_id }` immediately; the run streams via `agent-run-event`. */
export function startAgentRun(
  conversationId: number,
  content: string,
  provider: string,
  model: string,
): Promise<{ run_id: number }> {
  return invoke<{ run_id: number }>("start_agent_run", {
    conversationId,
    content,
    provider,
    model,
  });
}

/** Cancel an active run (Task 5.1). */
export function cancelAgentRun(runId: number): Promise<void> {
  return invoke<void>("cancel_agent_run", { runId });
}

/** Resolve a parked approval (Task 5.1; M1-core adds optional group scope). */
export function resolveAgentApproval(
  runId: number,
  callId: string,
  approved: boolean,
  scope?: "single" | "group",
): Promise<void> {
  return invoke<void>("resolve_agent_approval", {
    runId,
    callId,
    approved,
    scope: scope ?? null,
  });
}

/** Grant additional iterations to a budget-parked run (Task 5.1). */
export function extendAgentRun(runId: number, extraSteps: number): Promise<void> {
  return invoke<void>("extend_agent_run", {
    runId,
    extraSteps,
  });
}

/** List the runs of one conversation (rehydration, Task 5.1). */
export function listAgentRuns(conversationId: number): Promise<AgentRun[]> {
  return invoke<AgentRun[]>("list_agent_runs", {
    conversationId,
  });
}

/** List the steps of one run (rehydration, Task 5.1). */
export function listAgentSteps(runId: number): Promise<AgentStep[]> {
  return invoke<AgentStep[]>("list_agent_steps", { runId });
}

// ---- Run inspector (WS-D.1, read-only) ----------------------------------
// One aggregate view over a run's WS-B/WS-C accessories. Payloads stay
// snake_case (serde `rename_all = "snake_case"`); the command arg is
// camelCase (`runId`), like every other agent command. Secret-free by
// construction: fixed-vocabulary strings, counters, ids, and Unix-seconds
// timestamps only — plus caller-chosen checkpoint labels, never message
// content, tool arguments, credentials, or SQL. Opt-in gaps read `null`
// (empty lists for snapshots/checkpoints/self-audit-free runs).

/** One captured position marker (WS-C.1): stage index, fixed-vocabulary role
 * and state, counters, and the Unix-seconds capture time. No content. */
export interface SnapshotView {
  stage_index: number;
  role: string;
  state: string;
  steps_taken: number;
  spent_micro_usd: number;
  audit_len: number;
  captured_at: number;
}

/** One named checkpoint (WS-C.1): caller-chosen label plus snapshot index. */
export interface CheckpointView {
  name: string;
  snapshot_index: number;
}

/** One self-audit finding (WS-C.3): fixed-vocabulary code plus subject marker. */
export interface ViolationView {
  code: string;
  subject: number | null;
}

/** Latest self-audit verdict (WS-C.3): pass flag plus violation codes only. */
export interface SelfAuditView {
  passed: boolean;
  violations: ViolationView[];
}

/** Read-only aggregate view of one run (WS-D.1 `inspect_run`). */
export interface RunInspection {
  run_id: number;
  state: string;
  stage: string | null;
  role: string | null;
  stages_entered: number;
  stages_total: number;
  steps_taken: number;
  spent_micro_usd: number | null;
  limit_micro_usd: number | null;
  max_steps: number | null;
  gate_decisions: string[];
  audit_len: number | null;
  snapshots: SnapshotView[];
  checkpoints: CheckpointView[];
  self_audit: SelfAuditView | null;
  started_at: number;
  finished_at: number | null;
}

/** Inspect one run via `inspect_run`: state, stage + role, budget counters,
 * gate decisions, snapshot/checkpoint markers, and the latest self-audit
 * verdict. Unknown runs fail with a secret-free not-found error. */
export function inspectRun(runId: number): Promise<RunInspection> {
  return invoke<RunInspection>("inspect_run", { runId });
}

// ---- Spend dashboard (read-only) --------------------------------------
// Per-run spend (steps, micro-USD, budget caps, % used) plus aggregate
// totals across every persisted run, from the existing `agent_runs`
// counters only. Payloads stay snake_case (serde
// `rename_all = "snake_case"`); the command arg is camelCase (`runId`),
// like every other agent command. Secret-free by construction: integer ids,
// counters, and float ratios only — never model names, content, credentials,
// or SQL. Missing spend data reads `null` (never an error); unknown runs
// fail with a secret-free not-found error.

/** Per-run spend view: persisted counters plus the derivable budget ratios. */
export interface SpendRunView {
  run_id: number;
  steps_taken: number;
  spent_micro_usd: number | null;
  limit_micro_usd: number | null;
  /** Step cap: in-memory only, never persisted — always `null` here. */
  max_steps: number | null;
  steps_pct: number | null;
  spend_pct: number | null;
}

/** Aggregate spend totals across every persisted run (saturating sums). */
export interface SpendTotals {
  runs: number;
  total_steps: number;
  total_spent_micro_usd: number;
  runs_with_spend: number;
  runs_with_limit: number;
}

/** One dashboard response: the requested run plus the cross-run totals. */
export interface SpendDashboard {
  run: SpendRunView;
  totals: SpendTotals;
}

/** Load the spend dashboard for one run via `spend_dashboard`. */
export function spendDashboard(runId: number): Promise<SpendDashboard> {
  return invoke<SpendDashboard>("spend_dashboard", { runId });
}

// ---- Activity feed (read-only) ----------------------------------------
// One batched aggregate over the persisted run history: capped recent-run
// metadata rows (newest first) plus cross-run spend totals. Rows carry ids,
// fixed-vocabulary labels, counters, and Unix-seconds timestamps only —
// never `final_content` / `error` text (backend omits those columns by
// construction), so feed rows render metadata + labels, never content
// snippets. Payloads stay snake_case; the command arg is camelCase
// (`limit`), like every other agent command.

/** One activity-feed row: secret-free metadata of one `agent_runs` row. */
export interface ActivityRun {
  run_id: number;
  conversation_id: number | null;
  model: string;
  mode: string;
  status: string;
  started_at: number; // seconds since unix epoch
  finished_at: number | null; // seconds since unix epoch
  total_steps: number;
  spent_micro_usd: number | null;
  limit_micro_usd: number | null;
}

/** One activity-feed response: capped rows plus cross-run totals. */
export interface ActivityFeed {
  runs: ActivityRun[];
  totals: SpendTotals;
}

/** Load the activity feed via `activity_feed`. `limit` selects how many
 * recent runs to include (backend clamps to 1..100, default 50); the totals
 * always cover every persisted run. */
export function activityFeed(limit?: number): Promise<ActivityFeed> {
  return invoke<ActivityFeed>("activity_feed", { limit: limit ?? null });
}

// ---- Agent permission rules (M1-core) ----------------------------------

/** One `permission_rules` row (M1-core). */
export interface PermissionRule {
  id: number;
  preset: string;
  tool_pattern: string;
  path_pattern: string | null;
  effect: "allow" | "ask" | "deny";
  priority: number;
}

/** Add one persistent permission rule (M1-core). */
export function addPermissionRule(
  preset: string,
  toolPattern: string,
  pathPattern: string | null,
  effect: "allow" | "ask" | "deny",
): Promise<number> {
  return invoke<number>("add_permission_rule", {
    preset,
    toolPattern,
    pathPattern,
    effect,
  });
}

/** Remove one persistent permission rule by id (M1-core). */
export function removePermissionRule(id: number): Promise<void> {
  return invoke<void>("remove_permission_rule", { id });
}

/** List persistent permission rules (M1-core). */
export function listPermissionRules(): Promise<PermissionRule[]> {
  return invoke<PermissionRule[]>("list_permission_rules");
}

// ---- Agent mode & pause (Task 5.2) ------------------------------------

/** Autonomy modes for the agent (Task 5.2). */
export type AutonomyMode = "supervised" | "semi_autonomous" | "full_autonomous";

/** Live-switch the autonomy mode of an active run (Task 5.2). */
export function agentSetMode(runId: number, mode: AutonomyMode): Promise<void> {
  return invoke<void>("agent_set_mode", {
    runId,
    mode,
  });
}

/** Pause an active run at the next step boundary (Task 5.2). */
export function pauseAgentRun(runId: number): Promise<void> {
  return invoke<void>("pause_agent_run", { runId });
}

/** Resume a paused run (Task 5.2). */
export function resumeAgentRun(runId: number): Promise<void> {
  return invoke<void>("resume_agent_run", { runId });
}

// ---- Task manager + autonomous mode ------------------------------------
// User-defined task lists with agent-executable steps, plus the bounded
// plan → act → verify → report loop driving the existing agent run path.
// Payloads stay snake_case (serde structs); command args are camelCase
// (`taskId`, `conversationId`, `maxSteps`), like every other command.

/** One `agent_tasks` row as persisted (v8 migration). */
export interface AgentTask {
  id: number;
  title: string;
  description: string | null;
  status: "pending" | "running" | "completed" | "failed" | "cancelled";
  conversation_id: number | null;
  provider: string | null;
  model: string | null;
  max_steps: number;
  current_step: number;
  total_steps: number;
  report: string | null;
  run_id: number | null;
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
}

/** One `agent_task_steps` row as persisted (v8 migration). */
export interface AgentTaskStep {
  id: number;
  task_id: number;
  seq: number;
  title: string;
  status: "pending" | "running" | "completed" | "failed" | "skipped" | "cancelled";
  result: string | null;
  run_id: number | null;
  started_at: number; // seconds since unix epoch
  finished_at: number | null;
}

/** One `agent-task-event` frame: secret-free lifecycle (ids + fixed
 * vocabulary only — results and reports reload through the list commands). */
export type AgentTaskEventPayload =
  | { type: "started"; task_id: number }
  | { type: "step_started"; task_id: number; seq: number }
  | { type: "step_finished"; task_id: number; seq: number; status: string }
  | { type: "finished"; task_id: number; status: string };

/** Create a task with its ordered steps. A backing `Task: <title>`
 * conversation is created when `conversationId` is `null`. Returns the
 * schema-assigned task id. */
export function createTask(
  title: string,
  description: string | null,
  conversationId: number | null,
  provider: string | null,
  model: string | null,
  steps: string[],
  maxSteps?: number,
): Promise<number> {
  return invoke<number>("create_task", {
    title,
    description: description ?? null,
    conversationId: conversationId ?? null,
    provider: provider ?? null,
    model: model ?? null,
    steps,
    maxSteps: maxSteps ?? null,
  });
}

/** List all tasks, most recently active first. */
export function listTasks(): Promise<AgentTask[]> {
  return invoke<AgentTask[]>("list_tasks");
}

/** List one task's steps, `seq` ascending. */
export function listTaskSteps(taskId: number): Promise<AgentTaskStep[]> {
  return invoke<AgentTaskStep[]>("list_task_steps", { taskId });
}

/** Rename/edit a non-running task. */
export function updateTask(
  taskId: number,
  title: string,
  description: string | null,
): Promise<void> {
  return invoke<void>("update_task", {
    taskId,
    title,
    description: description ?? null,
  });
}

/** Delete a non-running task (its steps cascade). */
export function deleteTask(taskId: number): Promise<void> {
  return invoke<void>("delete_task", { taskId });
}

/** Start the autonomous loop for a task. Returns immediately; progress
 * streams via `agent-task-event`. */
export function startTaskRun(taskId: number): Promise<void> {
  return invoke<void>("start_task_run", { taskId });
}

/** Stop the active loop for a task (and abort its in-flight agent run).
 * Returns whether a loop was active. */
export function stopTaskRun(taskId: number): Promise<boolean> {
  return invoke<boolean>("stop_task_run", { taskId });
}

// ---- Kanban edit locks (P2 kanban+locks) -----------------------------------
// Lightweight per-task locks over the SAME task rows: acquire before a
// kanban move, present the token on `moveTask`, release afterwards. Every
// lock carries a deadline (default 120s, range 10..600s — never eternal);
// an expired row is stealable by the next acquirer. Payloads stay snake_case.

/** One `task_locks` row as persisted (v10 migration). */
export interface TaskLock {
  task_id: number;
  holder: string;
  token: string;
  acquired_at: number; // seconds since unix epoch
  expires_at: number; // seconds since unix epoch (deadline, always > acquired_at)
}

/** Acquire the edit lock for a task. Contention with a live lock by another
 * holder rejects with a `conflict`-kind `CommandError` naming the holder
 * and the deadline; an expired row is stolen. */
export function acquireTaskLock(
  taskId: number,
  holder: string,
  ttlSecs?: number,
): Promise<TaskLock> {
  return invoke<TaskLock>("acquire_task_lock", {
    taskId,
    holder,
    ttlSecs: ttlSecs ?? null,
  });
}

/** Release the edit lock for a task. The token must match the stored row. */
export function releaseTaskLock(taskId: number, token: string): Promise<void> {
  return invoke<void>("release_task_lock", { taskId, token });
}

/** Extend a live lock's deadline by a fresh TTL. Never resurrects dead
 * locks: a missing row, a mismatch, or an expired row rejects. */
export function heartbeatTaskLock(
  taskId: number,
  token: string,
  ttlSecs?: number,
): Promise<TaskLock> {
  return invoke<TaskLock>("heartbeat_task_lock", {
    taskId,
    token,
    ttlSecs: ttlSecs ?? null,
  });
}

/** Move a task to a kanban status (`pending` | `completed` | `failed` |
 * `cancelled`). The token must match the live lock row and be unexpired —
 * stale, mismatched, or missing locks reject and change nothing. */
export function moveTask(
  taskId: number,
  status: "pending" | "completed" | "failed" | "cancelled",
  token: string,
): Promise<void> {
  return invoke<void>("move_task", { taskId, status, token });
}

// ---- Context panel (read-only stats + diff render) ----------------------

/** Read-only per-conversation context stats (`conversation_context_stats`).
 * Token sums are `0` with `has_token_data: false` ("n/a") where no persisted
 * usage exists; nothing is ever estimated silently. Timestamps are Unix
 * seconds. */
export interface ConversationContextStats {
  conversation_id: number;
  title: string;
  provider: string | null;
  provider_display: string | null;
  model: string | null;
  context_limit: number;
  total_tokens: number;
  input_tokens: number;
  output_tokens: number;
  reasoning_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  has_token_data: boolean;
  total_cost_micro_usd: number;
  usage_percent: number;
  message_count: number;
  user_message_count: number;
  assistant_message_count: number;
  tool_call_count: number;
  other_step_count: number;
  created_at: number;
  updated_at: number;
}

/** Load one conversation's read-only context stats via
 * `conversation_context_stats`. */
export function conversationContextStats(
  conversationId: number,
): Promise<ConversationContextStats> {
  return invoke<ConversationContextStats>("conversation_context_stats", {
    conversationId,
  });
}

// ---- Agent workspace folder (1.3.0) ------------------------------------
// The agent's filesystem tools are scoped to one workspace root
// (`agent.workspace_root` setting, canonicalized backend-side). The recent
// list (`agent.workspace_recent`) holds at most 5 canonical paths,
// most-recent first.

/** Effective workspace root for tool scoping (`get_workspace_root`). */
export function getWorkspaceRoot(): Promise<string> {
  return invoke<string>("get_workspace_root");
}

/** Validate, canonicalize, persist `path` as the active root and register it
 * (`set_workspace_root` — runs the identical validation as `roots_add`,
 * including the nesting refusal). Joins the registry and the 5-entry picker
 * history. Returns the canonical path. */
export function setWorkspaceRoot(path: string): Promise<string> {
  return invoke<string>("set_workspace_root", { path });
}

/** Recent workspace roots, most-recent first (at most 5). */
export function listWorkspaceRecent(): Promise<string[]> {
  return invoke<string[]>("list_workspace_recent");
}

// ---- Multi-root registry (workspace roots switcher) --------------------
// The registry spans multiple repo roots: `active` is the effective root
// every root-aware feature follows (git panel, audit, terminal, agent runs,
// GitHub lists, flags, diagnostics); `roots` lists every registered root,
// active first. Registration validates backend-side (must exist,
// canonicalized, disjoint — nesting refused) into an unbounded registry
// (registering never evicts); removal never deletes
// directories, and removing the active root falls back to the default.

/** Multi-root registry view: the active root plus every registered root. */
export interface RootsList {
  active: string;
  roots: string[];
}

/** List the registered roots with the active root first (`roots_list`). */
export function rootsList(): Promise<RootsList> {
  return invoke<RootsList>("roots_list");
}

/** Register `path` as a root and make it active (`roots_add`).
 * Returns the updated registry view. */
export function rootsAdd(path: string): Promise<RootsList> {
  return invoke<RootsList>("roots_add", { path });
}

/** Unregister `path` from the registry (`roots_remove`). Removing the
 * active root falls back to the default. Returns the updated view. */
export function rootsRemove(path: string): Promise<RootsList> {
  return invoke<RootsList>("roots_remove", { path });
}

// ---- Workspace project directory (`.nexora/`) --------------------------
// Per-workspace Nexora home: a `nexora.json` manifest (forward-only
// version), workspace-relative ignore rules, and a `profiles/` scaffold.
// Created on explicit init only — never implicitly — and workspace-scoped
// files take precedence over app-global settings where both exist.

/** Initialize the workspace `.nexora/` project directory (`nexora_init`).
 * Idempotent: existing files are never overwritten. Returns the `.nexora/`
 * directory path. */
export function nexoraInit(): Promise<string> {
  return invoke<string>("nexora_init");
}

/** Persist one workspace-scoped routing profile (`save_workspace_profile`).
 * `task` selects `.nexora/profiles/chat.json` vs `agent.json`; `document`
 * is the JSON array of `{provider, model}` entries. The backend validates
 * before writing and refuses invalid documents secret-free. Returns the
 * written file path. */
export function saveWorkspaceProfile(task: "chat" | "agent", document: string): Promise<string> {
  return invoke<string>("save_workspace_profile", { task, document });
}

// ---- Feature flags (2.0 rollout) --------------------------------------
// Read-only rollout gates: workspace `.nexora/flags.json` wins over the
// app-global `flags.*` settings keys, which win over the hardcoded defaults
// (current behavior). There is no setter command: flags are edited where
// they live (the workspace file, or the `flags.*` keys via settings).

/** One flag's status in the read-only `flags_status` view. */
export interface FlagStatus {
  enabled: boolean;
  source: "workspace" | "global" | "default";
  /** Whether the run path enforces this flag today (phased rollout:
   * `injection`/`assembly` only; `snapshots`/`self_audit` resolve but gate
   * nothing yet). */
  enforced: boolean;
}

/** Read-only feature-flag status (`flags_status`): every registered flag
 * mapped to its effective value, fixed-vocabulary source, and enforcement
 * mark, plus the workspace-file fallback notice (`null` unless a present
 * workspace flags file failed to load). */
export interface FlagsStatus {
  flags: Record<string, FlagStatus>;
  notice: string | null;
}

export function flagsStatus(): Promise<FlagsStatus> {
  return invoke<FlagsStatus>("flags_status");
}

// ---- In-app docs (migration guide backbone) ---------------------------
// Read-only manifest behind the Docs panel: the ordered migration spine
// computed backend-side from the registered migrations (commands/docs.rs),
// so the guide lists exactly the migrations that exist. Payloads stay
// snake_case like every other backend struct. The per-version notes render
// frontend-side from the string catalog (`docs.migN`).

/** One schema milestone in the migration guide: the migration version only. */
export interface DocsMigration {
  version: number;
}

/** Docs manifest (`docs_manifest`): highest known schema version plus the
 * full ordered migration spine. */
export interface DocsManifest {
  schema_version: number;
  migrations: DocsMigration[];
}

export function docsManifest(): Promise<DocsManifest> {
  return invoke<DocsManifest>("docs_manifest");
}

// ---- Version control (git inspection, guarded writes, timeline) --------
// Git inspection for the opened workspace: branch + changed files + recent
// commits (with per-commit stats and risk signals) in one `git_info` round
// trip, per-file unified diffs via `git_file_diff` (lazy, server-side capped
// with a truncation notice), per-commit diffs via `git_commit_diff` (lazy,
// same cap shape), guarded writes (`git_stage` / `git_unstage` / `git_commit`
// / `git_push`, each with an explicit per-call confirmation), and two AI
// assists through the existing execution path (`git_generate_commit_message`
// for staged changes, `git_explain_commit` for one historical commit — both
// keyring-only, nothing persisted, copy-only results).
// Payloads stay snake_case like every other backend struct; command args are
// camelCase (`limit`, `path`, `hash`, `commitHash`).

/** One changed file: repository-relative path plus a fixed-vocabulary
 * status (`"modified"`, `"staged"`, `"untracked"`, `"deleted"`, `"renamed"`). */
export interface GitFileStatus {
  path: string;
  status: string;
}

/** One recent commit: full hash plus summary, author name, Unix-seconds
 * time, per-commit file statistics, and heuristic risk signals. The message
 * is the commit summary (first line) only. `risk_signals` carries zero or
 * more of the fixed vocabulary `"large-diff"`, `"many-files"`, `"binary"`,
 * `"merge-commit"`, `"unfamiliar-author"` (computed locally, no AI). */
export interface GitCommit {
  hash: string;
  message: string;
  author: string;
  time: number; // seconds since unix epoch
  files_changed: number;
  insertions: number;
  deletions: number;
  risk_signals: string[];
}

/** Aggregate read-only git view: current branch (`null` when detached or
 * unborn), changed files sorted by path, and recent commits newest-first.
 * `files` is capped server-side (500, sorted order) with any remainder
 * reported in `files_overflow` (a count only, never content). */
export interface GitInfo {
  branch: string | null;
  files: GitFileStatus[];
  files_overflow: number;
  commits: GitCommit[];
}

/** One per-file unified diff, capped server-side (256 KiB) with a truncation
 * notice. Binary content reports `binary` with an empty diff. */
export interface GitFileDiff {
  path: string;
  diff: string;
  truncated: boolean;
  binary: boolean;
}

/** Load the aggregate git view for the effective workspace root via
 * `git_info`. `limit` selects how many recent commits to include (backend
 * clamps to 1..100); the panel passes a small page such as 20. */
export function gitInfo(limit?: number): Promise<GitInfo> {
  return invoke<GitInfo>("git_info", { limit: limit ?? null });
}

/** Load one file's unified diff via `git_file_diff`. `path` is the
 * repository-relative path from `GitInfo.files`; traversal attempts fail
 * with a fixed-vocabulary error. */
export function gitFileDiff(path: string): Promise<GitFileDiff> {
  return invoke<GitFileDiff>("git_file_diff", { path });
}

/** One historical commit's unified diff (`git_commit_diff`): the full hash
 * as resolved, the capped diff text, and the changed repository-relative
 * paths (sorted, capped server-side with the remainder in
 * `files_overflow`). Binary deltas are omitted with a placeholder line;
 * `binary` reports whether any delta was binary. */
export interface GitCommitDiff {
  hash: string;
  diff: string;
  truncated: boolean;
  binary: boolean;
  files: string[];
  files_overflow: number;
}

/** Load one historical commit's unified diff via `git_commit_diff`. `hash`
 * is the full hash from `GitInfo.commits` (prefixes are never guessed);
 * malformed or unknown hashes fail with a fixed-vocabulary error. */
export function gitCommitDiff(hash: string): Promise<GitCommitDiff> {
  return invoke<GitCommitDiff>("git_commit_diff", { hash });
}

/** Stage `paths` (repository-relative) into the index via `git_stage`.
 * Returns how many were staged. The wrapper always passes the explicit
 * per-call confirmation the backend requires for writes. */
export function gitStage(paths: string[]): Promise<number> {
  return invoke<number>("git_stage", { paths, confirmed: true });
}

/** Unstage `paths` (repository-relative) back to `HEAD` via `git_unstage`.
 * Returns how many were unstaged; confirmed like `gitStage`. */
export function gitUnstage(paths: string[]): Promise<number> {
  return invoke<number>("git_unstage", { paths, confirmed: true });
}

/** Commit the staged index with `message` via `git_commit`. Returns the new
 * commit hash. The backend validates the message and refuses without the
 * explicit confirmation this wrapper always passes. */
export function gitCommit(message: string): Promise<string> {
  return invoke<string>("git_commit", { message, confirmed: true });
}

/** Push the current branch to `origin` via `git_push` (never forced; only
 * `origin` is accepted backend-side). Confirmed like `gitStage`. */
export function gitPush(): Promise<void> {
  return invoke<void>("git_push", { remote: "origin", confirmed: true });
}

/** AI-generated commit message for the staged changes
 * (`git_generate_commit_message`). `truncated_input` reports whether the
 * staged summary fed to the model was truncated server-side. */
export interface GeneratedCommitMessage {
  message: string;
  truncated_input: boolean;
}

// ---- Workspace terminal (user-authored commands via the agent tool path) -
// One thin command area over the existing `execute_command` tool:
// `terminal_run` dispatches a real tool call (workspace-scoped cwd,
// hard timeout, bounded capture, truncation with notice) and returns its
// combined output with `truncated`/`success` display flags;
// `terminal_kill` cancels the active run's token (the executor kills the
// child). Single session: at most one run is active. Payloads stay
// snake_case; command args are camelCase (`command`, `cwd`,
// `confirmationId`), like every other command.

/** Combined output of one finished terminal run (`terminal_run`).
 * `success` is false when the tool path rendered its non-zero-exit
 * marker; the status text stays inside `output` (no structured code
 * crosses IPC). `truncated` mirrors the tool path's truncation notice
 * inside `output`. Timestamps: none (runs are session-only). */
export interface TerminalRunResult {
  run_id: number;
  output: string;
  truncated: boolean;
  success: boolean;
}

/** Run one workspace command via `terminal_run`. The call blocks until the
 * tool path returns (completion, timeout kill, or stop kill).
 *
 * Clicking Run *requests* the approval; the backend then raises a blocking
 * NATIVE OS confirmation dialog showing this exact command and working
 * directory, and mints a single-use id only if the user accepts. The id is
 * bound to this command + cwd, so it cannot authorize a different run.
 * Cancelling at the native dialog rejects and runs nothing — the panel shows
 * the message and no output block is marked failed.
 *
 * `cwd` is workspace-relative (`null` = workspace root); absolute escape is
 * refused backend-side, and a refused `cwd` does not consume the id.
 */
export async function terminalRun(command: string, cwd: string | null): Promise<TerminalRunResult> {
  const confirmationId = await requestConfirmation("terminal", command, { cwd });
  return invoke<TerminalRunResult>("terminal_run", {
    command,
    cwd,
    confirmationId,
  });
}

/** Stop the active terminal run via `terminal_kill`. Returns whether a run
 * was active (and is now cancelled); `false` means nothing was running. */
export function terminalKill(): Promise<boolean> {
  return invoke<boolean>("terminal_kill");
}

/** AI diagnosis of one failed terminal run (`terminal_explain`).
 * `explanation` says what went wrong, `suggested_fix` is copy-only text
 * (never auto-applied — the user copies it or retypes the command
 * manually). `truncated_input` reports whether the failed output fed to
 * the model was truncated server-side. */
export interface ErrorExplanation {
  explanation: string;
  suggested_fix: string;
  truncated_input: boolean;
}

/** Explain one failed terminal run via `terminal_explain`, using the
 * existing AI execution path (keyring-only credentials, nothing
 * persisted). `exitContext` is the short display line for the failure
 * (e.g. the exit badge text); `null` sends output only. */
export function terminalExplain(
  output: string,
  exitContext: string | null,
  provider: string,
  model: string,
): Promise<ErrorExplanation> {
  return invoke<ErrorExplanation>("terminal_explain", {
    output,
    exitContext,
    provider,
    model,
  });
}

/** Generate a conventional-commit message for the staged changes via
 * `git_generate_commit_message`, using the existing AI execution path
 * (keyring-only credentials, nothing persisted). */
export function gitGenerateCommitMessage(
  provider: string,
  model: string,
): Promise<GeneratedCommitMessage> {
  return invoke<GeneratedCommitMessage>("git_generate_commit_message", {
    provider,
    model,
  });
}

/** AI explanation of one historical commit (`git_explain_commit`).
 * `explanation` says what changed and why it matters; it is copy-only text
 * (never auto-applied — the user copies it manually). `truncated_input`
 * reports whether the commit diff fed to the model was truncated
 * server-side. */
export interface CommitExplanation {
  explanation: string;
  truncated_input: boolean;
}

/** Explain one historical commit via `git_explain_commit`, using the
 * existing AI execution path (keyring-only credentials, nothing
 * persisted). `commitHash` is the full hash from `GitInfo.commits`. */
export function gitExplainCommit(
  commitHash: string,
  provider: string,
  model: string,
): Promise<CommitExplanation> {
  return invoke<CommitExplanation>("git_explain_commit", {
    commitHash,
    provider,
    model,
  });
}

// ---- GitHub issues & PRs (read-only panel, github.com only) --------------
// Two batch commands over the workspace `origin` repo: `gh_issues` lists
// issues (pull-request rows excluded backend-side) and `gh_pulls` lists
// pull requests, each one capped page (at most 50 items) for the `state`
// filter (`open` / `closed` / `all`, default `open`). Read-only end to end:
// GET only, no commenting/labeling/merging anywhere on the path. The token
// resolves backend-side from the OS keyring entry `github` and never
// crosses IPC — a missing token is not an error (`authenticated: false`,
// panel shows its connect-hint; the shared unauthenticated quota applies).
// Payloads stay snake_case like every other backend struct; the command arg
// is camelCase (`state`), like every other command.

/** Rate-limit snapshot echoed from the response headers (`null` when the
 * header was absent — render as "unknown", never as zero). */
export interface GhRateLimit {
  limit: number | null;
  remaining: number | null;
  reset: number | null;
}

/** One issue row: fixed-vocabulary metadata plus the capped body. */
export interface GhIssue {
  number: number;
  title: string;
  state: string;
  author: string;
  labels: string[];
  comments: number;
  created_at: string | null;
  updated_at: string | null;
  html_url: string | null;
  body: string;
  body_truncated: boolean;
}

/** One pull-request row: fixed-vocabulary metadata plus the capped body. */
export interface GhPull {
  number: number;
  title: string;
  state: string;
  author: string;
  draft: boolean;
  head_ref: string | null;
  base_ref: string | null;
  comments: number;
  created_at: string | null;
  updated_at: string | null;
  html_url: string | null;
  body: string;
  body_truncated: boolean;
}

/** State filter vocabulary for both list commands. */
export type GhState = "open" | "closed" | "all";

/** One `gh_issues` response: resolved repo, filter echo, capped items,
 * token presence, and the rate-limit snapshot. A quota-exhausted API
 * answers with empty `items` and `rate_limited: true` (never a silent
 * empty, never an error dump). */
export interface GhIssuesResult {
  owner: string;
  repo: string;
  state: string;
  items: GhIssue[];
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** One `gh_pulls` response: same envelope as `GhIssuesResult`. */
export interface GhPullsResult {
  owner: string;
  repo: string;
  state: string;
  items: GhPull[];
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** List the workspace origin repo's issues via `gh_issues`. `state`
 * selects `open` / `closed` / `all` (default `open`). */
export function ghIssues(state?: GhState): Promise<GhIssuesResult> {
  return invoke<GhIssuesResult>("gh_issues", { state: state ?? null });
}

/** List the workspace origin repo's pull requests via `gh_pulls`. */
export function ghPulls(state?: GhState): Promise<GhPullsResult> {
  return invoke<GhPullsResult>("gh_pulls", { state: state ?? null });
}

// ---- GitHub Actions fail-loop (read-only CI surface, same panel) ---------
// Two commands over the workspace `origin` repo. `gh_actions` lists the
// recent workflow runs (at most 20, newest first) with failing runs expanded
// (failed jobs metadata only — failed step names, no log bodies, so one
// batch call never fans out into dozens of log downloads). `gh_action_log`
// fetches one job's capped, secret-scrubbed log tail on job-expand (the last
// 200 lines under 24000 chars; oversized downloads keep their tail). An
// expired or missing log resolves to `log_unavailable`; an auth refusal
// (401/403) to `log_needs_auth`. Read-only end to end: GET only, no
// re-running anywhere on the path (the user re-runs on GitHub; the panel
// points at the run URL). The fix loop creates a task-manager task prefilled
// with the failure context through the existing `createTask` wrapper —
// creating the task starts nothing; the user reviews and runs it from Tasks
// with approvals and budgets intact.
// Payloads stay snake_case like every other backend struct.

/** One failed CI job inside a failing workflow run: fixed-vocabulary
 * metadata and failed step names. Log bodies are lazy: `gh_actions` ships
 * jobs with an empty `log_excerpt` and all log flags false, and the panel
 * fetches each log on job-expand via `gh_action_log`. */
export interface GhActionJob {
  id: number;
  run_id: number;
  name: string;
  status: string;
  conclusion: string;
  started_at: string | null;
  completed_at: string | null;
  html_url: string | null;
  failed_steps: string[];
  log_excerpt: string;
  log_truncated: boolean;
  log_unavailable: boolean;
  log_needs_auth: boolean;
  log_redacted: boolean;
}

/** One workflow run: fixed-vocabulary metadata plus the failed jobs (only
 * failing runs expand; other runs carry an empty `failed_jobs` list).
 * `jobs_truncated` marks omitted failed jobs beyond the kept few. */
export interface GhActionRun {
  id: number;
  name: string;
  head_branch: string | null;
  head_sha: string | null;
  event: string;
  status: string;
  conclusion: string;
  created_at: string | null;
  updated_at: string | null;
  html_url: string | null;
  failed_jobs: GhActionJob[];
  jobs_truncated: boolean;
}

/** One `gh_actions` response: resolved repo, recent runs, the API's total
 * run count, token presence, and the rate-limit snapshot. A quota-exhausted
 * API answers with empty `runs` and `rate_limited: true` (never a silent
 * empty, never an error dump). */
export interface GhActionsResult {
  owner: string;
  repo: string;
  runs: GhActionRun[];
  total_runs: number;
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** List the workspace origin repo's recent workflow runs via `gh_actions`. */
export function ghActions(): Promise<GhActionsResult> {
  return invoke<GhActionsResult>("gh_actions");
}

/** One lazily fetched job log via `gh_action_log`: the capped,
 * secret-scrubbed tail of one failed job. `log_unavailable` means the log
 * was expired, never uploaded, or its fetch failed (see the run on GitHub
 * instead); `log_needs_auth` means the endpoint refused for auth reasons
 * (sign in first); `log_redacted` flags a scrubbed excerpt. */
export interface GhActionLog {
  job_id: number;
  run_id: number;
  log_excerpt: string;
  log_truncated: boolean;
  log_unavailable: boolean;
  log_needs_auth: boolean;
  log_redacted: boolean;
}

/** Fetch one failed job's log tail via `gh_action_log`. */
export function ghActionLog(runId: number, jobId: number): Promise<GhActionLog> {
  return invoke<GhActionLog>("gh_action_log", { runId, jobId });
}

// ---- GitHub runners detect (read-only detect surface, same panel) --------
// One batch command over the workspace `origin` repo: `gh_runners` lists the
// repo's self-hosted runners (at most 30, with label names) plus the local
// readiness checklist (`git` + `gh` + `docker` presence via `PATH` lookup —
// never executed). Listing self-hosted runners needs an admin-scoped token,
// so an anonymous or under-scoped read resolves to empty `runners` with
// `runners_needs_auth: true` (the panel shows its needs-token state, never
// an error dump). Detection only: no registration, removal, or dispatch
// exists anywhere on this path. Payloads stay snake_case like every other
// backend struct; the command takes no arguments.

/** One self-hosted runner: fixed-vocabulary metadata plus label names. */
export interface GhRunner {
  id: number;
  name: string;
  os: string;
  status: string;
  busy: boolean;
  labels: string[];
}

/** One local readiness entry: whether the tool resolves on `PATH`. */
export interface GhToolReadiness {
  name: string;
  found: boolean;
}

/** One `gh_runners` response: resolved repo, self-hosted runners, the API's
 * total runner count, the needs-token flag, the local readiness checklist,
 * token presence, and the rate-limit snapshot. */
export interface GhRunnersResult {
  owner: string;
  repo: string;
  runners: GhRunner[];
  runners_total: number;
  runners_needs_auth: boolean;
  local: GhToolReadiness[];
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** List the workspace origin repo's self-hosted runners plus the local
 * readiness probe via `gh_runners` (detection only). */
export function ghRunners(): Promise<GhRunnersResult> {
  return invoke<GhRunnersResult>("gh_runners");
}

// ---- Repository audit (read-only static analysis, findings only) ---------
// One batch command over the workspace sources: `repo_audit` scans `.rs` /
// `.ts` / `.tsx` files under the effective workspace root (capped server-side
// with skip notices) and returns fixed-vocabulary findings with `file:line`
// evidence plus capped excerpts. Findings never modify code — there is no
// auto-fix path; results are copy/read-only. Payloads stay snake_case like
// every other backend struct; the command takes no arguments.

/** One audit finding: fixed-vocabulary kind + severity with `file:line`
 * evidence (workspace-relative `path`, 1-based `line`) and a capped code
 * excerpt (at most 3 lines). */
export interface AuditFinding {
  kind: string;
  severity: string;
  path: string;
  line: number;
  excerpt: string;
}

/** One file the scan did not read: path plus a fixed-vocabulary reason
 * (`"too-large"`, `"unreadable"`, `"file-cap"`). */
export interface SkippedFile {
  path: string;
  reason: string;
}

/** One read-only audit run: capped findings plus scan accounting. Totals
 * always cover the whole walk; lists stay capped with overflow counts. */
export interface RepoAuditReport {
  findings: AuditFinding[];
  findings_overflow: number;
  files_scanned: number;
  files_skipped: number;
  skipped: SkippedFile[];
  skipped_overflow: number;
}

/** Run the read-only repository audit via `repo_audit`. Manual runs only —
 * the panel never watches or re-audits live. */
export function repoAudit(): Promise<RepoAuditReport> {
  return invoke<RepoAuditReport>("repo_audit");
}

// ---- Test drafts (template-generated Rust #[test] scaffolds, review buffer)
// One batch command over the workspace sources: `testgen_drafts` runs the
// read-only audit, then derives template-based drafts for undocumented or
// unused public Rust functions (capped server-side with an overflow count).
// Drafts are response data only — nothing is written; the user copies them
// manually. Payloads stay snake_case like every other backend struct; the
// command takes no arguments.

/** One draft test scaffold: arrange/act/assert skeleton with honest TODO
 * markers (`fn_name` tested as `test_<fn_name>`; `signature` is the
 * declaration line the scaffold was derived from). */
export interface TestDraft {
  path: string;
  line: number;
  fn_name: string;
  source_kind: string;
  signature: string;
  code: string;
}

/** One read-only testgen run: capped drafts plus generation accounting. */
export interface TestgenReport {
  drafts: TestDraft[];
  drafts_overflow: number;
  targets_considered: number;
  files_scanned: number;
  audit_overflow: number;
}

/** Generate draft test scaffolds via `testgen_drafts`. Manual runs only —
 * the panel never generates live. */
export function testgenDrafts(): Promise<TestgenReport> {
  return invoke<TestgenReport>("testgen_drafts");
}

// ---- Dependency inventory (read-only lockfile tables) -------------------
// One batch command over the workspace lockfiles: `dep_inventory` reads
// `src-tauri/Cargo.lock` (cargo table) and `package-lock.json` (npm table)
// and returns locked name/version/source rows, capped server-side with
// overflow counts. Tables report locked (resolved) versions only — declared
// ranges from the manifests never appear. No install/update/remove path
// exists anywhere in this slice: the inventory never writes.

/** One locked dependency: resolved name + version plus a fixed-vocabulary
 * source label (cargo: `"crates.io"` | `"git"` | `"local"` | `"unknown"`;
 * npm: `"registry"` | `"git"` | `"local"` | `"unknown"`). */
export interface DepEntry {
  name: string;
  version: string;
  source: string;
}

/** One read-only inventory run: capped cargo + npm tables with totals.
 * Totals always cover the whole lockfile; lists stay capped with overflow
 * counts. A missing or unparsable lockfile yields an empty table. */
export interface DepInventory {
  cargo: DepEntry[];
  cargo_total: number;
  cargo_overflow: number;
  npm: DepEntry[];
  npm_total: number;
  npm_overflow: number;
}

/** Load the dependency inventory via `dep_inventory`. Manual loads only —
 * the panel never watches the lockfiles. */
export function depInventory(): Promise<DepInventory> {
  return invoke<DepInventory>("dep_inventory");
}

// ---- Safe refactor apply (confirmed dead-code removals only) ------------
// One write command over a single workspace file range: `refactor_apply`
// deletes workspace-relative `.rs` lines `[startLine, endLine]` (1-based,
// inclusive) for a finding of an allowlisted kind (`dead-code-candidate`
// only — every other audit kind refuses). The backend re-verifies every
// guard before writing: explicit confirmation, kind allowlist, git-repo
// presence, tracked-and-clean file, usable range, and a `pub `-declaration
// first line. The file was clean beforehand, so `git checkout -- <path>`
// reverts. NOTE: command ARGS are camelCase (Tauri v2); payloads stay
// snake_case — never align one to the other.

/** One applied refactor: what was removed, the re-read file shape, and the
 * post-apply verification flag. */
export interface RefactorApplyResult {
  path: string;
  start_line: number;
  end_line: number;
  removed_lines: number;
  file_lines: number;
  removed_preview: string;
  verified: boolean;
}

/** Apply one confirmed dead-code removal via `refactor_apply`. The wrapper
 * always passes the explicit per-call confirmation the backend requires
 * for writes — ticking the panel's confirmation box IS the approval. */
export function refactorApply(
  path: string,
  startLine: number,
  endLine: number,
  kind: string,
): Promise<RefactorApplyResult> {
  return invoke<RefactorApplyResult>("refactor_apply", {
    path,
    startLine,
    endLine,
    kind,
    confirmed: true,
  });
}

// ---- Debt backlog (manual entries + idempotent audit imports) -----------
// Five commands behind the Debt panel: `create_debt_item` adds one manual
// entry; `list_debt_items` returns every row (most recently touched first);
// `update_debt_item_status` moves one row through the triage lifecycle
// (`open` / `accepted` / `fixed` / `wontfix`); `delete_debt_item` removes
// one row; `import_debt_from_audit` runs the read-only audit over the
// workspace and inserts fresh findings as rows (re-runs insert nothing new).
// NOTE: command ARGS are camelCase (Tauri v2); payloads stay snake_case —
// never align one to the other.

/** One `debt_items` row as persisted (v10 migration). */
export interface DebtItem {
  id: number;
  title: string;
  source: "audit" | "manual";
  severity: "info" | "warning";
  status: "open" | "accepted" | "fixed" | "wontfix";
  location: string | null;
  note: string | null;
  audit_key: string | null;
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
}

/** Add one manually tracked debt entry. Returns the schema-assigned id. */
export function createDebtItem(
  title: string,
  severity: string,
  location: string | null,
  note: string | null,
): Promise<number> {
  return invoke<number>("create_debt_item", {
    title,
    severity,
    location: location ?? null,
    note: note ?? null,
  });
}

/** List every debt item, most recently touched first. */
export function listDebtItems(): Promise<DebtItem[]> {
  return invoke<DebtItem[]>("list_debt_items");
}

/** Move one debt item to a new triage status. */
export function updateDebtItemStatus(debtId: number, status: string): Promise<void> {
  return invoke<void>("update_debt_item_status", { debtId, status });
}

/** Delete one debt item by id (a no-op when the id is unknown). */
export function deleteDebtItem(debtId: number): Promise<void> {
  return invoke<void>("delete_debt_item", { debtId });
}

/** Outcome of one audit import: newly inserted rows plus rows skipped as
 * unimportable (`inserted == 0 && skipped == 0` means already tracked). */
export interface DebtImportResult {
  inserted: number;
  skipped: number;
}

/** Import current audit findings as debt rows. Returns inserted + skipped
 * counts (0 + 0 when everything was already tracked). */
export function importDebtFromAudit(): Promise<DebtImportResult> {
  return invoke<DebtImportResult>("import_debt_from_audit");
}

// ---- Diagnostics / update check / pre-update snapshot ---------------------
// Three read-mostly commands behind the health panel's System section:
// `diagnostics_bundle` collects the secret-free bundle (versions, platform,
// storage counts, swept crash rows — never content, credentials, or paths),
// `update_check` compares the running build against the workspace origin
// repo's latest GitHub release (one GET — check only, no download or
// install; the panel renders the release URL as text), and
// `snapshot_database` writes one user-initiated `VACUUM INTO` copy under the
// app-data `backups` directory before an update (never silent, never on a
// timer). Payloads stay snake_case like every other backend struct.

/** Storage counters in the diagnostics bundle: row counts only. */
export interface BundleCounts {
  conversations: number;
  messages: number;
  prompts: number;
  agent_runs: number;
  agent_steps: number;
  agent_tasks: number;
}

/** One retained crash row: startup-sweep rows only (id, model, timestamp). */
export interface CrashedRun {
  run_id: number;
  model: string;
  started_at: number;
}

/** Secret-free diagnostics bundle: versions, platform, storage, crashes. */
export interface DiagnosticsBundle {
  app_version: string;
  platform_os: string;
  platform_arch: string;
  schema_version: number;
  schema_target: number;
  db_size_bytes: number | null;
  counts: BundleCounts;
  fts_present: boolean;
  crashed_runs: CrashedRun[];
  error_runs: number;
}

/** Collect the secret-free diagnostics bundle via `diagnostics_bundle`. */
export function diagnosticsBundle(): Promise<DiagnosticsBundle> {
  return invoke<DiagnosticsBundle>("diagnostics_bundle");
}

/** One update check: running build vs the origin repo's latest release. */
export interface UpdateCheck {
  owner: string;
  repo: string;
  current_version: string;
  latest_tag: string | null;
  latest_name: string;
  latest_url: string | null;
  published_at: string | null;
  update_available: boolean;
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** Check the origin repo's latest release via `update_check` (GET only). */
export function updateCheck(): Promise<UpdateCheck> {
  return invoke<UpdateCheck>("update_check");
}

/** A pre-update snapshot copy: file name (never the path) plus size. */
export interface SnapshotInfo {
  file_name: string;
  size_bytes: number;
  created_at: number;
}

/** Write one user-initiated pre-update snapshot via `snapshot_database`. */
export function snapshotDatabase(): Promise<SnapshotInfo> {
  return invoke<SnapshotInfo>("snapshot_database");
}

// ---- Release readiness + issue automation ---------------------------------
// Two commands behind the Release panel (operator close-the-loop surface):
// `release_status` aggregates the honest readiness facts — manifest version
// parity (`package.json` / `src-tauri/Cargo.toml` /
// `src-tauri/tauri.conf.json` below the workspace root; missing manifests
// surface as `null`, never an error), migration facts (known vs applied vs
// pending), the newest pre-update snapshot presence, and the local `gh` CLI
// probe. Read-only and network-free; no new tables. The update signal stays
// on `update_check` (it needs the network) — the panel calls both.
// `create_issue_for_finding` files one caller-confirmed issue through the
// user's own `gh` CLI (`gh issue create` with `cwd` = workspace root, so the
// repo resolves itself). Auth stays in the CLI: no token ever crosses IPC —
// only the confirmed title/location/detail/source strings travel, and only
// title/body strings reach the subprocess as argv. The per-call `confirmed`
// flag carries the destructive-action confirmation pattern (bare IPC callers
// cannot file). Payloads stay snake_case like every other backend struct;
// command args are camelCase (`debtId` style) like every other command.

/** Newest pre-update snapshot: file name (never the path) plus size. */
export interface ReleaseSnapshot {
  file_name: string;
  size_bytes: number;
}

/** Release readiness facts, all computed backend-side (never hand-waved).
 * The panel maps these to pass/warn/fail badges. */
export interface ReleaseStatus {
  app_version: string;
  package_json_version: string | null;
  cargo_version: string | null;
  tauri_conf_version: string | null;
  versions_agree: boolean;
  migration_count: number;
  schema_version: number;
  schema_target: number;
  pending_migrations: number;
  snapshot: ReleaseSnapshot | null;
  gh_available: boolean;
  gh_version: string | null;
}

/** Collect the release readiness facts via `release_status`. */
export function releaseStatus(): Promise<ReleaseStatus> {
  return invoke<ReleaseStatus>("release_status");
}

/** Fixed-vocabulary issue source for the link-back line. */
export type IssueSource = "debt" | "audit" | "release-check";

/** One filed issue: the canonical URL `gh` printed plus its issue number. */
export interface CreatedIssue {
  url: string;
  number: number;
}

/** File one caller-confirmed issue via `create_issue_for_finding`.
 * `confirmed` must be true (the dialog owns the confirmation UX). */
export function createIssueForFinding(
  title: string,
  source: IssueSource,
  confirmed: boolean,
  location?: string | null,
  detail?: string | null,
): Promise<CreatedIssue> {
  return invoke<CreatedIssue>("create_issue_for_finding", {
    title,
    location: location ?? null,
    detail: detail ?? null,
    source,
    confirmed,
  });
}

// ---- Privacy center (telemetry controls + local usage ledger) -------------
// Three commands behind the health panel's Privacy section:
// `privacy_status` (every egress surface with its live state + the ledger
// aggregates), `privacy_export` (the same aggregates as copy/download JSON),
// and `privacy_wipe` (user-confirmed delete behind `confirmed`). Payloads
// stay snake_case like every other backend struct. There is no telemetry
// upload anywhere: the ledger never leaves the machine except through the
// user's own copy/download of this export.

/** Live state of one egress surface. */
export type PrivacySurfaceState = "on" | "off" | "manual" | "local";

/** One egress surface: where data can leave, its live state, honest detail. */
export interface PrivacySurface {
  id: string;
  title: string;
  destination: string;
  state: PrivacySurfaceState;
  detail: string;
}

/** One aggregated ledger row: event kind + day bucket + count. Counts only. */
export interface LedgerStat {
  kind: string;
  day: number;
  count: number;
}

/** Privacy status: the surface inventory plus ledger aggregates. */
export interface PrivacyStatus {
  surfaces: PrivacySurface[];
  stats: LedgerStat[];
  total_events: number;
  retention_days: number;
  oldest_day: number | null;
}

/** Ledger export: aggregates plus metadata, shaped for copy/download. */
export interface LedgerExport {
  exported_at: number;
  retention_days: number;
  stats: LedgerStat[];
}

/** Result of a confirmed ledger wipe: how many rows were deleted. */
export interface WipeResult {
  deleted_rows: number;
}

/** Collect the privacy status via `privacy_status` (read-only, local). */
export function privacyStatus(): Promise<PrivacyStatus> {
  return invoke<PrivacyStatus>("privacy_status");
}

/** Export the ledger aggregates via `privacy_export` (counts only). */
export function privacyExport(): Promise<LedgerExport> {
  return invoke<LedgerExport>("privacy_export");
}

/** Wipe every ledger row via `privacy_wipe` (needs `confirmed: true`). */
export function privacyWipe(confirmed: boolean): Promise<WipeResult> {
  return invoke<WipeResult>("privacy_wipe", { confirmed });
}
