# Agent Run Streaming Bridge + Steps Accordion UI — Design (Task 5.1.0)

Status: **design only — no production code, no migrations, no IPC/UI
implementation in this task.** Coordinator turns this into the 5.1
implementation prompt; the Human resolves the DECISION POINTS table at the end.

Repo state at design time: `origin/main` HEAD `1f8dcf1` (single-commit
history; the repo squashes on promote), 312 Rust tests green. Verified
backend surface: `AgentRunner::run`
(`src-tauri/src/application/agent/runner.rs:326-357`), governance-only
`AgentRunEvent` (`src-tauri/src/application/agent/control.rs:79-119`), the
opt-in `RunRecorder` (`src-tauri/src/application/agent/persistence.rs:71-98`),
and the complete `agent_runs`/`agent_steps` repository
(`src-tauri/src/infrastructure/repository/agent_runs.rs:122-280`). Every
anchor in this document was read and verified; none is phantom.

Two module docs explicitly defer to this task:

- `control.rs:73-77`: "Per-step streaming events are out of scope here
  (Task 5.1)"; `control.rs:23`: events are "wired to the Tauri layer in
  Milestone 5".
- `persistence.rs:13-14`: runs start with `conversation_id = NULL`; the
  "Task 5.1 IPC layer will begin wiring runs to conversations" (D50).

Out of scope (5.2+): terminal/diff viewer, autonomy switch UI, approval
polish, settings UI. 6.x: E2E. This design must let 5.2 **extend, not
rewrite**: the registry, event stream, and accordion are built so 5.2 adds
controls (autonomy switch, richer approval cards) over the same surfaces.

---

## 1. Step event model

### 1.1 Evaluation: extend `AgentRunEvent` vs a separate side-channel enum

| Option | Verdict | Rationale |
|---|---|---|
| **(A) Extend `AgentRunEvent` with step variants, emitted by the recorder** | **RECOMMENDED** | One `mpsc` channel already exists end to end: the runner accepts `std::sync::mpsc::Sender<AgentRunEvent>` (`with_event_sender`) and `emit()` is best-effort (`runner.rs:531-535`). Governance events and step events then share **one total order** for free. The recorder already knows `run_id` and `seq` at exactly the points where steps are persisted, so live events and `agent_steps` rows are produced by the same code path — seq alignment is structural, not maintained by hand. |
| (B) Separate `StepEvent` enum on a second channel | Rejected | Two channels = two partial orders. The bridge would need a merge (by what key? there is no shared timestamp with usable resolution) and the UI would need to interleave governance and step events heuristically. Strictly worse for zero gain. |

### 1.2 Where step events are emitted: the recorder, not the runner

The runner's emit sites (`runner.rs:427-430`, `runner.rs:447`,
`runner.rs:473-495`, `runner.rs:484`, `runner.rs:540`) do **not** know
`run_id`, and they do not know `seq` — the recorder owns both
(`ActiveRunRecord`, `persistence.rs:172-177`), and `seq` deliberately does
**not advance when persistence fails** (CF-01, `persistence.rs:341-409`).
Emitting from the runner would desync live events from persisted steps in
exactly that failure case.

Therefore: `RunRecorder`/`ActiveRunRecord` gain an optional
`Sender<AgentRunEvent>` and emit one event per **successfully persisted**
step. The runner itself is **untouched** (zero behavior change; see §7).

Rule (emission-after-success): a step event is emitted only when the
`agent_steps` insert succeeded. Consequences, all accepted:

- Live `seq` values are always valid and aligned with `agent_steps.seq`
  (no duplicates ever, because a failed insert reuses its `seq` — CF-01 —
  and emits nothing).
- The live stream may omit steps whose persistence failed. The UI treats
  the live stream as best-effort display; **rehydration (§6.5) is the
  source of truth** after the run ends.
- Runs executed without a recorder emit no step events. The 5.1 IPC
  start command always attaches a recorder (§3.2), so this case is not
  reachable from the UI.

### 1.3 Exact extension of `AgentRunEvent`

One new variant (mirrors the persisted `AgentStep` row,
`agent_runs.rs:77-98`, which is already `Serialize`):

```rust
/// One successfully persisted agent step, streamed to the UI (Task 5.1).
/// Emitted by the recorder immediately after the `agent_steps` insert
/// succeeds; `seq` is exactly the persisted value (CF-01-aligned).
StepRecorded {
    /// The run this step belongs to.
    run_id: i64,
    /// 1-based sequence within the run, identical to `agent_steps.seq`.
    seq: i64,
    /// `'model_turn' | 'tool_call' | 'approval'` (schema CHECK values).
    kind: String,
    /// Tool name; `None` for `model_turn`.
    tool_name: Option<String>,
    /// Raw JSON arguments exactly as provider-supplied.
    arguments: Option<String>,
    /// Model-turn content / tool output / denial or approval text.
    observation: Option<String>,
    /// `'succeeded' | 'failed' | 'denied' | 'cancelled'` (tool/approval only).
    status: Option<String>,
    /// Step duration in milliseconds (model turn: provider round trip;
    /// tool call: dispatch duration), when known.
    duration_ms: Option<i64>,
},
```

Design notes:

- **Completed-only, no started/completed pairs.** The recorder records
  model turns *after* the provider returns (`runner.rs:408-414`) and tool
  calls *after* dispatch (`runner.rs:509-525`) — D12 semantics. Mirroring
  that in the stream means one event per step, no half-open state to
  reconcile, and rehydration replays the identical shape. Live
  "something is happening" feedback is the run status pill + spinner
  (§6.3) plus `ApprovalRequested` (which already fires *before* the park,
  `runner.rs:473-477`). Live `ModelTurnStarted` / `ToolCallStarted`
  events are explicitly deferred to 5.2 (DECISION POINT 2).
- **`content` vs `observation`:** one field, `observation`, reusing the
  persisted column's meaning (`agent_runs.rs:90`: "Tool output / denial
  text / approval decision"; for `model_turn` it carries the assistant
  narration, same as `rec.model_turn(&response.content, ..)` at
  `runner.rs:413`). Live and rehydrated payloads are identical.
- **Governance events unchanged.** `Paused`/`Resumed`/`BudgetExhausted`/
  `SpendLimitExceeded`/`ApprovalRequested`/`ApprovalResolved`/`Cancelled`/
  `Completed { steps }` keep their exact shapes (`control.rs:79-119`).
  Adding an enum variant is additive; existing matches in the runner and
  its tests are on specific variants, not exhaustive, so they compile
  unchanged.
- **Terminal events.** `Completed { steps }` is *not* extended with
  `run_id`/`final_content` — that would break every existing construction
  site and re-classify a governance event as a payload carrier. Instead
  the bridge emits its own terminal frame (§2.4):

  ```rust
  /// Bridge-level terminal frame (not part of AgentRunEvent): emitted by
  /// the run thread after `run()` returns, before its Sender is dropped.
  struct RunFinished {
      run_id: i64,
      conversation_id: i64,
      /// 'completed' | 'cancelled' | 'budget_exhausted'
      /// | 'spend_limit_exceeded' | 'error'   (agent_runs.rs:37-38 values)
      status: String,
      final_content: Option<String>,  // 'completed' only
      error: Option<String>,          // 'error' only, classified, never a secret
  }
  ```

  The status/error mapping is the same one `ActiveRunRecord::finalize`
  already applies (`persistence.rs:26-31`), so the UI, the stream, and
  `agent_runs` cannot disagree.
- **Ordering guarantees.** Single unbounded `std::sync::mpsc` channel; one
  sender clone held by the runner (`with_event_sender`) and one by the
  recorder (§1.2). `mpsc` preserves FIFO; all senders feed the same
  channel, so governance and step events arrive in emission order. Event
  flood is structurally bounded: ≤ `max_iterations` model turns + a
  like number of tool calls + ≤ 2 events per parked approval — small,
  text-only payloads; no backpressure mechanism is needed.


---

## 2. Bridge & threading

### 2.1 Spawn model (the runner is synchronous and must never block the UI)

The existing `send_message` command already established the threading
doctrine: the blocking pipeline must leave the async runtime/IPC thread
(`commands/conversations.rs:97-100` documents the BUG-005 scheduling
constraint). For agent runs the constraint is stronger — a run is
*long-lived and cancellable*, not a one-shot request — so:

- **One dedicated `std::thread::spawn` per run** (not
  `tauri::async_runtime::spawn_blocking`): a blocking-pool task is not
  addressable afterwards, while a named thread's lifecycle is owned by the
  registry and survives arbitrary pause/approval parks. The thread calls
  `AgentRunner::run(...)` (`runner.rs:326`) and dies with it.
- **One forwarder thread per run**, owning the `mpsc::Receiver` and doing
  nothing but `app.emit("agent-run-event", payload)` per event. The run
  thread never touches `AppHandle` (keeps the application layer free of
  Tauri types via a sink trait, §2.3).
- **Frontend never blocks:** the start command returns immediately (§3.2);
  the run thread parks, waits, and emits independently.

### 2.2 Active-run registry (Tauri managed state)

New file `src-tauri/src/commands/agent_runs.rs` + application service
`src-tauri/src/application/agent/service.rs`. Registry is Tauri managed
state, wired in `lib.rs` next to the existing `Database` manage call
(`lib.rs:83`):

```rust
/// Managed state: every in-flight agent run, keyed by run_id.
pub(crate) struct AgentRunRegistry(Mutex<HashMap<i64, ActiveAgentRun>>);

pub(crate) struct ActiveAgentRun {
    pub conversation_id: i64,
    /// Cancel/pause/extend handle (`control.rs:144-165`). `cancel()`
    /// wakes parked approval and budget waits (`control.rs:197-200`).
    pub control: RunControl,
    /// Present when the run is gated (§5); `respond()` resolves a park
    /// (`approval.rs:246-256`).
    pub gate: Option<ApprovalGate>,
    pub model: String,
    pub started_at: i64,
}
```

Lifecycle:

1. **Insert** happens in `start_agent_run` *before* the thread spawns, so
   `cancel_agent_run` can never race a not-yet-registered run.
2. **Removal** happens on the run thread in a scope guard, immediately
   after `run()` returns and after the terminal `RunFinished` frame is
   emitted — termination of any kind (success, cancel, error, park-abort)
   cleans up. A panic in `run()` (none expected; the runner is
   panic-free by design, AC-9/AC-10) would leak the entry until app exit;
   the guard covers the normal paths, which is sufficient for 5.1.
3. **Registry queries:** `cancel_agent_run` and
   `resolve_agent_approval` look up by `run_id`; the per-conversation
   uniqueness check (§2.5) scans for `conversation_id`.


### 2.3 Receiver loop and the sink boundary

To keep Tauri types out of the application layer, the bridge is defined as
a trait the command layer implements:

```rust
// application/agent/service.rs
pub(crate) trait RunEventSink: Send + 'static {
    fn emit(&self, event: &AgentRunEvent);
    fn emit_finished(&self, finished: &RunFinished);
}

// commands/agent_runs.rs — thin IPC glue only (commands/mod.rs:1-13 doctrine)
struct TauriRunEventSink(AppHandle);
impl RunEventSink for TauriRunEventSink {
    fn emit(&self, event: &AgentRunEvent) {
        let _ = self.0.emit("agent-run-event",
            AgentRunEventPayload::Governance { run_id, event: event.clone() });
    }
    fn emit_finished(&self, finished: &RunFinished) {
        let _ = self.0.emit("agent-run-event",
            AgentRunEventPayload::Finished { run_id: finished.run_id,
                event: finished.clone() });
    }
}
```

The forwarder thread body is deliberately trivial and total:

```rust
// drains until every Sender is dropped — this IS the queue-draining rule
while let Ok(event) = rx.recv() {
    sink.emit(&event);
}
// recv() returns Err only when the channel is disconnected, i.e. the run
// thread (holding the last Sender clones) has exited after emitting
// RunFinished. Nothing is dropped silently; disconnect is the drain
// guarantee.
```

**Queue draining before thread exit:** the run thread emits `RunFinished`
*then* drops its senders; `recv()` keeps returning buffered events until
the channel disconnects, so the forwarder always flushes every queued
event before exiting. No timeout, no drop path.

**Registry cleanup on termination** is independent of the forwarder: the
guard on the run thread removes the entry after `RunFinished` is sent, so
`cancel_agent_run` on a just-finished run simply misses (NotFound mapping,
§3.4) instead of cancelling a dead handle.

### 2.4 Event naming: one channel name, `run_id` in the payload

**RECOMMENDED: a single event name `agent-run-event` for every frame**
(step, governance, terminal), with `run_id` as a payload field:

```ts
// frontend payload discriminator (serde-tagged on the backend)
type AgentRunEventPayload =
  | { type: "step";       run_id: number; event: StepRecordedPayload }
  | { type: "governance"; run_id: number; event: GovernanceEventPayload }
  | { type: "finished";   run_id: number; event: RunFinishedPayload };
```

Rejected alternative — per-run channel names (`agent-run-{id}`): forces a
dynamic `listen()`/`unlisten()` round-trip per run, races the first events
(listen is async; events emitted before the listener registers are lost),
and complicates mockBackend parity. One static name + client-side
`run_id` filter has none of those problems and survives
conversation-switch rehydration trivially (§6.5).

Tauri event permission: `core:default` is already granted in
`src-tauri/capabilities/default.json:7`; it bundles the core event
permissions (listen/emit). **No capability change required.**

### 2.5 Concurrency policy

**RECOMMENDED: at most one active run per conversation; runs in different
conversations may proceed concurrently.**

- One run per conversation prevents interleaved assistant messages and
  two accordions fighting over one thread view. `start_agent_run` rejects
  with a classified error when the registry holds an active run for the
  same `conversation_id` (§3.4).
- Cross-conversation concurrency is safe today: step persistence
  serializes on the shared `Mutex<Connection>` (`Database::lock`, the seam
  every repository already uses), and each run owns its channel, thread,
  and registry entry.
- App-wide serialization (one run at a time) is simpler but artificially
  blocks parallel work; it costs nothing to allow and can be tightened
  later (DECISION POINT 4).


---

## 3. IPC commands

New module `src-tauri/src/commands/agent_runs.rs`, registered in
`generate_handler![...]` (`lib.rs:28-66`) and wrapped in `src/lib/tauri.ts`
per the three-step doctrine (AGENTS.md). Commands stay thin: they resolve
state, delegate to the application service
(`application/agent/service.rs`), and map errors
(`commands/error.rs:57-76` doctrine: no credentials, raw SQL, or payload
content in messages).

### 3.1 Signatures

```rust
/// Start an opt-in agent run for a conversation. Returns immediately.
#[tauri::command]
pub(crate) fn start_agent_run(
    conversation_id: i64,
    content: String,          // becomes the user message
    provider: String,         // provider NAME ('openai'|'anthropic'|'gemini')
    model: String,            // must be in the provider's SUPPORTED_MODELS
    attachment_ids: Vec<i64>, // drafts to link, same semantics as send_message
    db: State<'_, Database>,
    registry: State<'_, AgentRunRegistry>,
    app: AppHandle,
) -> Result<StartAgentRunResponse, CommandError>;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct StartAgentRunResponse {
    pub run_id: i64,
}

/// Cancel a run (works while running, paused-ineligible, budget-parked,
/// or approval-parked: `RunControl::cancel` wakes every waiter,
/// `control.rs:197-200`).
#[tauri::command]
pub(crate) fn cancel_agent_run(
    run_id: i64,
    registry: State<'_, AgentRunRegistry>,
) -> Result<(), CommandError>;

/// Resolve a parked approval (5.1 minimum; 5.2 polishes the UX).
#[tauri::command]
pub(crate) fn resolve_agent_approval(
    run_id: i64,
    call_id: String,
    approved: bool,
    registry: State<'_, AgentRunRegistry>,
) -> Result<(), CommandError>;

/// Grant `extra_steps` more iterations to a budget-parked run
/// (the "Continue" affordance; see §5 for why this must ship in 5.1).
#[tauri::command]
pub(crate) fn extend_agent_run(
    run_id: i64,
    extra_steps: u32,
    registry: State<'_, AgentRunRegistry>,
) -> Result<(), CommandError>;

/// Rehydration: runs of one conversation, `started_at` DESC
/// (`agent_runs.rs:241-254`).
#[tauri::command]
pub(crate) fn list_agent_runs(
    conversation_id: i64,
    db: State<'_, Database>,
) -> Result<Vec<AgentRun>, CommandError>;

/// Rehydration: the steps of one run, `seq` ASC (`agent_runs.rs:330-334`).
#[tauri::command]
pub(crate) fn list_agent_steps(
    run_id: i64,
    db: State<'_, Database>,
) -> Result<Vec<AgentStep>, CommandError>;
```

Note: **no `pause_agent_run` command in 5.1.** Without it the `Paused`
state is unreachable from the UI (the runner only parks on pause when
`RunControl::pause` was called, `runner.rs:549-560`), so no parked-without-
resolver state exists via pause. Pause/resume controls are 5.2.

### 3.2 `start_agent_run` behavior, step by step

1. Validate inputs (non-empty content; provider/model against the same
   supported lists the UI already consumes via `supported_providers`).
2. Verify the conversation exists and no active run exists for it (§2.5).
3. **Resolve the credential inside the backend** via the existing
   `CredentialStore` path (the same resolution `RequestExecutionService`
   performs for plain chat, `application/conversations.rs:19,118-119`).
   The credential string is passed to `AgentRunner::run`
   (`runner.rs:326-332`) and **never serialized, logged, or emitted** —
   it exists only inside the spawned thread's closure. The frontend
   never sees it, matching the keyring doctrine (DATABASE.md §14,
   ARCHITECTURE §9).
4. Build the run stack, all opt-in and identical in shape to what tests
   already exercise: `AgentRunner::new(..)`
   `.with_control(RunControl::new())`
   `.with_approval_gate(gate, mode)` (mode per DECISION POINT 3)
   `.with_event_sender(tx.clone())`
   `.with_run_recorder(recorder_with_conversation)` (§4).
5. Insert the registry entry (§2.2), spawn the run thread + forwarder
   thread (§2.1), and return `{ run_id }` immediately. The user message
   is persisted *before* the thread starts (next step), so a crash can
   never lose it.
6. **User message persistence** reuses the `ConversationService`
   persistence path (`application/conversations.rs:161-191` flow):
   persist the user message + link draft attachments exactly as
   `send_message` does, factored as a small `ConversationService`
   method (`persist_user_message`) so the two paths cannot drift.
7. **Assistant answer persistence** happens on the run thread after
   `run()` returns `Ok(content)`: persist via the same service
   (`persist_assistant_message`, provider + model recorded like plain
   chat), then emit `RunFinished`. On any `Err`, **no assistant message
   is created** — the same doctrine as plain chat
   (`useConversation.ts:9-13`: "never manufactures a fake assistant
   message"); the run's final content lives in `agent_runs.final_content`
   and the accordion, not in `messages`.


### 3.3 Threading note (regression guard)

Like `send_message` (BUG-005, `commands/conversations.rs:97-100` and its
negative-control test), `start_agent_run` itself must do no blocking work:
it persists the user message synchronously (fast, local SQLite — same as
plain chat does inside its command) and delegates everything long-lived to
the spawned threads. `run()`'s reqwest::blocking stack executes on a plain
`std::thread`, which is outside the async-runtime worker prohibition.

### 3.4 `CommandError` mapping rules

Follow `commands/error.rs` exactly (classified `ErrorKind` + curated
message; `error.rs:1-9, 57-76`). New mappings:

| Source | Mapping |
|---|---|
| Conversation does not exist | `NotFound`, "conversation {id} does not exist" (same wording as `error.rs:207`) |
| Unknown provider / model not in SUPPORTED_MODELS | `InvalidInput`, "unsupported provider or model" (no catalog detail needed) |
| Missing keyring credential | `Credential`, via the existing `From<CredentialError>` mapping (`error.rs:23`) — same UX as plain chat's fail-before-send |
| Another run active for this conversation | `InvalidInput`, "a run is already active for this conversation" |
| Run not in registry (finished/unknown id) for cancel/extend/resolve | `NotFound`, "run {id} is not active" |
| Approval `call_id` mismatch / no pending request | `NotFound`, "no pending approval for call {call_id}" (gate `respond` returned false, `approval.rs:246-256`) |
| `AgentError::Provider/EmptyResponse` at persist-assistant time | **not surfaced as a command error** — the run already emitted `RunFinished { status: "error", error }`; nothing to reject |
| SQLite failures anywhere | `Database` via the existing curated mapping (`error.rs:86+`) |

Hard rules restated: credentials never cross IPC or enter a message; raw
SQL and file paths never enter messages; tool observations contain tool
output only (workspace tool output is not secret-bearing, and it is the
same text already persisted to `agent_steps` — §1.3).

---

## 4. `conversation_id` wiring (recorder)

No schema change: `agent_runs.conversation_id` exists and is `NULL`
today (`agent_runs.rs:43-45`), `create_run` already accepts
`Option<i64>` (`agent_runs.rs:138-150`), and the D50 cascade (conversation
delete ⇒ runs+steps delete) is already schema-enforced and tested
(`agent_runs.rs:519-542`).

Change (application layer only):

- `RunRecorder::new(db)` → `RunRecorder::with_conversation(db,
  conversation_id: i64)` (or a second constructor keeping `new` for the
  NULL case — the smallest-diff option is a field + updated
  `insert_run`, `persistence.rs:86-98`, which currently hardcodes
  `create_run(None, model, mode)` at `persistence.rs:88`).
- The recorder reference flows through the existing
  `with_run_recorder` builder into `ActiveRunRecord::start`
  (`runner.rs:337-343`); no runner signature changes.
- Existing call sites to update: the single production construction point
  in the new 5.1 service (passes the real id) and the recorder unit tests
  (`persistence.rs:354`, currently `RunRecorder::new(&db)`).

Rehydration is then pure repository reads, both already present:

- `list_runs_by_conversation(conversation_id)` → runs newest-first
  (`agent_runs.rs:241-254`);
- `list_steps(run_id)` → ordered by `seq` ascending (`ORDER BY seq`,
  `agent_runs.rs:330-334`; gap-free per CF-01, exercised at
  `persistence.rs:394-401`).

The UI joins them client-side (§6.5); no new repository methods are
needed for 5.1.


---

## 5. Approval UX timing — no parked run without a resolver

### 5.1 The risk, precisely

`request_approval` parks the run thread until `respond` or cancellation
(`approval.rs:206-241`; the 20 ms poll + shared token make the wait
cancellation-safe). The autonomy switch and polished approval controls are
**5.2**. If 5.1 attaches a gate in `Supervised` mode, the very first tool
call (`read_file` is `ReadOnly`, `approval.rs:40`) parks a run the user
cannot resolve — a silent, indefinite hang behind a spinner. That state is
**unacceptable** and the design must make it unreachable.

### 5.2 Options

| Option | Assessment |
|---|---|
| (a) Attach gate + ship minimal `resolve_agent_approval` IPC + bare inline Approve/Deny in the accordion | Full ladder behavior immediately; 5.2 only polishes presentation and adds the autonomy switch. Requires shipping two extra commands (`resolve_agent_approval`, already in §3.1) and one inline UI block. |
| **(b′) Attach gate in `SemiAutonomous` + ship the same minimal resolver** | **RECOMMENDED — this is (a) with a safer default mode.** `SemiAutonomous` auto-executes `ReadOnly` tools (`read_file`, `list_directory` — workspace-root-bounded by the registry contract, `runner.rs:4-21`) and parks only for `Mutating` calls (`write_file`, `execute_command`, unknown tools — `approval.rs:26-44`). Parks are rare, always visible (`ApprovalRequested` precedes the park, `runner.rs:473-477`), and always resolvable (§5.4). |
| (c) No gate in 5.1 (`FullAutonomous`-equivalent) | Rejected: 5.1 would auto-execute `write_file`/`execute_command` with zero user control — a product regression relative to the HD-3 ladder (4.1) shipped precisely to prevent this. |

The gate-free *mode* question ("state exactly which tools/modes can run
gate-free and why that is safe"): only `ReadOnly` tools run gate-free, and
only under `SemiAutonomous`, because the registry bounds them to the
configured workspace root and they cannot mutate state
(`runner.rs:15-21`). `Mutating` tools never run gate-free in any mode
below `FullAutonomous`, and 5.1 never selects `FullAutonomous`.

### 5.3 The other parked state: budget exhaustion

With `RunControl` attached (required for cancel), an exhausted budget
**parks** the run until `extend_steps` or `cancel`
(`control.rs:84-89,260-271`; `runner.rs` honor-allowance gate). If 5.1
ships cancel but not extend, a budget park is a second
parked-without-resolver state. Therefore `extend_agent_run`
(§3.1) + a "Continue" affordance ship in 5.1 (DECISION POINT 5). Cancel
alone would also resolve it, but killing a run the user wanted to continue
is worse than one small command.

### 5.4 Invariant: every park has ≥ 1 resolver

Enumerated exhaustively for the 5.1 command surface:

| Parked state | Resolvers in 5.1 |
|---|---|
| Approval park (`ApprovalRequested`) | `resolve_agent_approval(run_id, call_id, approved)` → `gate.respond` (`approval.rs:246`); **or** `cancel_agent_run` → `RunControl::cancel` wakes the parked wait → `request_approval` returns `Err` → run aborts as `cancelled` (`runner.rs:478-486`) |
| Budget park (`BudgetExhausted`) | `extend_agent_run` → `RunControl::extend_steps` (`control.rs:223-229`); **or** `cancel_agent_run` |
| User pause | **Unreachable in 5.1** — no `pause_agent_run` command exists (§3.1 note); 5.2 adds pause together with its resume control |
| Agent-applied timeout / provider stall | Not a park: `request_timeout` bounds each provider call (`runner.rs:402`); the thread returns, `RunFinished` fires |

Rule enforced by tests (§8): *after any 5.1 UI flow, a parked run always
has an enabled IPC resolver, and cancel is accepted in every state while
the run is in the registry.*

---

## 6. UI: steps accordion

### 6.1 Files

- `src/components/AgentRunSteps.tsx` — the accordion (presentational;
  follows existing component conventions: function component, `nex-*`
  class names, `aria-*` on interactive parts, like `ConversationView.tsx`).
- `src/lib/useAgentRun.ts` — data hook (all backend access via `src/lib/`,
  mirroring `useConversation.ts` placement and structure).
- `src/lib/tauri.ts` — typed wrappers + event payload types (invoke-only
  file today, `tauri.ts:9`; event listening is added here as the single
  import site of `@tauri-apps/api/event`, so components never import the
  SDK directly — same layering rule as `invoke`).
- `src/lib/mockBackend.ts` — dev-mode parity (§6.6).

### 6.2 Placement

- **During a run:** the accordion renders in the thread below the last
  message, in the position the typing indicator occupies today
  (`ConversationView.tsx:179-189`) — it is, effectively, a structured
  typing indicator. The user message for the run is already persisted
  (§3.2 step 6), so it appears above the accordion via the normal
  history reload.
- **After the run:** when the assistant message is persisted
  (`completed` runs only), the accordion collapses to a compact
  summary row directly beneath that assistant message, keyed by
  `run_id`; expandable to the full step list at any time. Association:
  each `agent_runs` row carries `started_at`/`finished_at`
  (`agent_runs.rs:52-55`); the accordion block is rendered
  chronologically interleaved with messages by timestamp, which keeps
  rehydration order stable without any new backend join
  (DECISION POINT 6 documents the alternative).
- Non-`completed` runs (cancelled/error/budget/spend) leave **no**
  assistant message (§3.2 step 7); their accordion stays where the run
  happened and shows the terminal pill.


### 6.3 Component shape

```tsx
// src/components/AgentRunSteps.tsx
export interface AgentRunStepsProps {
  run: AgentRunView;            // run metadata + steps + live status
  onResolveApproval: (callId: string, approved: boolean) => void;
  onCancel: () => void;
  onContinue: (extraSteps: number) => void;  // budget park only
}

export interface AgentRunView {
  run_id: number;
  status: "running" | "completed" | "cancelled" | "budget_exhausted"
        | "spend_limit_exceeded" | "error";   // agent_runs.rs:37-38 values
  model: string;
  started_at: number;
  finished_at: number | null;
  error: string | null;            // classified text only
  steps: AgentStepView[];          // unified: live + rehydrated
  pending_approval: { call_id: string; name: string; arguments: string } | null;
}
```

Rendering rules:

- **Per-step sections, grouped by kind** — `model_turn | tool_call |
  approval` — each collapsible, defaulting: collapsed for
  `model_turn` (the final answer already shows in the thread),
  expanded for the latest step while running, collapsed for older
  `tool_call` steps. Section header shows: seq, kind icon, tool name or
  "Model turn", duration_ms when present, status chip
  (`succeeded/failed/denied/cancelled` — the persisted values,
  `agent_runs.rs:74`).
- **Step body:** `observation` text (same content as persisted, §1.3);
  `arguments` rendered as monospace JSON (the existing
  `nex-tag-mono` treatment). 5.2 replaces this body with the
  terminal/diff viewer for `write_file`; the accordion exposes
  `kind + tool_name + arguments` today so 5.2 swaps the body renderer
  only.
- **Approval block (bare, 5.1):** when `pending_approval` is set:
  tool name + arguments + Approve / Deny buttons →
  `resolveAgentApproval(run_id, call_id, approved)`. This is the minimal
  resolvable UI required by §5; 5.2 polishes it into a proper card.
- **Run status pill** (`nex-morph-pill` treatment like the composer send
  button, `ConversationView.tsx:271-277`): `running` (spinner, matches
  `nex-spinner`), `completed`, `cancelled`, `budget_exhausted` (+ the
  Continue control → `extendAgentRun(run_id, 10)`), `spend_limit_exceeded`,
  `error` (+ the classified `error` message). A `running` pill also shows
  Cancel (→ `cancelAgentRun`).

### 6.4 Hook design: `useAgentRun`

```ts
// src/lib/useAgentRun.ts — mirrors useConversation.ts structure
export function useAgentRun(conversationId: number | null): {
  runs: AgentRunView[];          // rehydrated + live, newest first
  reload: () => Promise<void>;   // re-fetch runs+steps for this conversation
}
```

- **Subscribe:** one `listen<AgentRunEventPayload>("agent-run-event", ...)`
  per hook instance (i.e. per mounted `ConversationView`), filtering
  `payload.run_id` against the runs of the active conversation. Events for
  other conversations are ignored (same staleness doctrine as
  `useConversation.ts:58-62`, via an `activeConversationRef`).
- **Live append:** `step` frames append/replace by `seq` (a duplicate
  seq is ignored — idempotent against double delivery);
  `governance` frames with `ApprovalRequested` set `pending_approval`,
  `ApprovalResolved` clears it; `finished` frames set the terminal
  status and trigger one `reload()` so persisted truth
  (`agent_runs`/`messages`) replaces the live view — replace, never
  patch-and-hope, matching the "history is replaced, never appended"
  doctrine (`useConversation.ts:77-78`).
- **Cleanup on unmount / conversation switch:** the `listen` promise's
  `unlisten` is called in the effect's cleanup; the `activeConversationRef`
  guard drops in-flight frames from a previous conversation. No timers,
  no polling — the stream is push-only.
- **Rehydration on conversation switch (§6.5):** effect keyed on
  `conversationId` calls `listAgentRuns(conversationId)`, then
  `listAgentSteps(runId)` for the visible window of runs (all of them in
  5.1; runs are small), building `AgentRunView[]`.


### 6.5 Rehydration semantics

1. `list_agent_runs(conversation_id)` (newest first) → for each run,
   `list_agent_steps(run_id)` (seq ASC) → `AgentRunView` with
   `status` straight from the persisted column — including any run that
   was in flight while the window reloaded (its persisted status is
   authoritative; a still-`running` row for a run whose thread died with
   the app is displayed as `error` with the classified text "run did not
   finish" — see §7 caveat).
2. Live frames for a *rehydrated* run (window stayed on one conversation)
   merge in by `seq`; after `finished`, `reload()` reconciles.
3. Runs with `conversation_id IS NULL` cannot exist once 5.1 ships
   (every UI-started run links its conversation, §4); pre-5.1 rows are
   simply never listed for a conversation.

### 6.6 mockBackend parity (dev mode)

`mockBackend.ts` already fakes the full IPC surface behind
`__TAURI_INTERNALS__` including `transformCallback` (`mockBackend.ts:312-326`).
Additive changes:

- Handle new commands: `start_agent_run` (persists the user message,
  then streams ~5 synthetic steps over ~4 s: two `model_turn`, one
  `tool_call` succeeded, one `approval` parked ~2 s then auto-resolved
  so dev QA sees the approval block without interaction, final
  `model_turn` + `finished`), `cancel_agent_run`,
  `resolve_agent_approval`, `extend_agent_run`, `list_agent_runs`,
  `list_agent_steps`.
- Deliver events through the mock event channel: implement
  `plugin:event|listen`/`unlisten` in the mock (store callbacks) and emit
  `agent-run-event` payloads on the same timing source, so the real
  `listen()` code path in `tauri.ts` is exercised unchanged in browser QA.
- Mock runs persist `agent_runs`-shaped objects in memory so
  rehydration renders identically after conversation switches.

### 6.7 Event flood mitigation (UI side)

- The accordion coalesces React state updates with a microtask batch
  (steps arrive at most one per provider round trip — naturally slow);
  no virtualization needed at 5.1 scale (`max_iterations` bounded).
- Long `observation` text (> ~2000 chars) is truncated in the collapsed
  body with the full text kept in state (5.2's terminal viewer will
  need it anyway).

---

## 7. Backward-compat & opt-in proof

Every existing behavior that must not change, and why the design cannot
change it:

1. **Plain chat is byte-identical.** `ConversationService::send_message`
   (`application/conversations.rs:161-191`), the `send_message` command
   (`commands/conversations.rs:92+`), `useConversation.send`
   (`useConversation.ts:102-132`), and the composer flow
   (`ConversationView.tsx:101-123`) are untouched. The only shared-code
   touch is *factoring out* user/assistant message persistence into
   service methods that `send_message` also calls — a pure refactor
   where the agent path becomes a second caller of the same functions.
   (If the Coordinator prefers zero refactor, the service can duplicate
   the two small inserts; DECISION POINT 7.)
2. **No fake assistant message on failure** — preserved by construction:
   the assistant message is persisted only after `run()` returns `Ok`
   (§3.2 step 7), identical doctrine to plain chat
   (`useConversation.ts:9-13`).
3. **Runner unchanged.** Zero edits to `runner.rs` for 5.1: step events
   come from the recorder (§1.2), conversation_id from the recorder
   (§4), governance from the existing emit sites. `run()` without
   recorder/control/gate/sender keeps exact Task 3.1 behavior
   (`runner.rs:309-310, 35-43, 51-52` doctrine).
4. **No defaults change.** `AgentRunner::new` still attaches nothing;
   `DEFAULT_RECORDED_MODE` stays `supervised` (`persistence.rs:47-50`);
   the recorder stays opt-in and best-effort (CF-01 policy intact — the
   added event emission is *after* the successful insert, so a failed
   insert emits nothing and advances nothing).
5. **`AgentRunEvent` is additively extended.** Existing variants keep
   names, fields, and emission points (`control.rs:79-119`); no existing
   match site breaks.
6. **Schema untouched.** No migration; `conversation_id` column,
   FK cascade, and CHECK constraints all pre-exist and are already
   tested (`agent_runs.rs:519-542, 545-607`).
7. **mock mode unaffected for existing commands.** All mockBackend
   changes are new `case` branches + a new event channel implementation;
   every existing case is untouched (`mockBackend.ts:88-310`).
8. **Agent path is opt-in per conversation.** The only way to start a
   run is the new `start_agent_run` command; nothing in the plain-chat
   UI invokes it until the 5.1 UI entry point (a per-conversation agent
   toggle or explicit affordance) is wired — the plain composer send
   path never mutates.
9. **Capabilities.** `core:default` (`capabilities/default.json:7`)
   already covers the event API; no permission additions, no
   `tauri.conf.json` changes.
10. **Caveat carried forward (documented, not fixed here):** a run whose
    thread dies with the app (crash/kill while parked) leaves a
    `running` row forever; 5.1 renders it as `error` client-side (§6.5).
    A startup sweep (`UPDATE agent_runs SET status='error' ... WHERE
    status='running'`) is a candidate for 5.2 — listed as DECISION
    POINT 8 rather than silently added (it is a write on startup, which
    this design does not authorize itself).


---

## 8. Test plan per layer

All Rust tests use the established in-memory SQLite pattern
(`in_memory_database()`, as at `persistence.rs:353` and throughout the
~150 existing tests). Frontend has no test framework — UI is verified
mock-driven via the existing Puppeteer visual-QA doctrine
(`mockBackend.ts:1-10`).

### 8.1 Event emission (recorder, `persistence.rs` + `control.rs`)

- **Seq/order:** a run with recorder + event sender records a model turn,
  a tool call, an approval; assert the channel receives exactly one
  `StepRecorded` per step, in emission order, with `seq` values `1..n`
  identical to `list_steps(run_id)` seqs, `run_id` correct, and every
  payload field equal to the persisted row.
- **Emission-after-success (CF-01 interaction):** occupy seq 1 to force
  the first insert to fail (same seam as `persistence.rs:352-365`):
  assert *no* event for the failed step, and the retry emits `seq = 1`
  after succeeding.
- **No sender attached:** recorder behaves exactly as today (events are
  the only delta).
- **No recorder attached:** no step events exist at all.
- **Governance pass-through:** runner emits the existing variants
  unchanged; a single channel fed by runner + recorder yields a total
  order with `ApprovalRequested` strictly before its `approval` step
  event.

### 8.2 Registry lifecycle (`application/agent/service.rs`)

- start → registry contains the run; terminal result → entry removed
  (success, cancel, provider error, budget park + extend, approval park
  + resolve — one test each).
- cancel before/at/after park states: `cancel_agent_run` resolves a
  parked approval (`request_approval` → `Err`) and a budget park.
- duplicate start for the same conversation rejected; different
  conversations run concurrently (two threads, both finalize).
- drain guarantee: buffered events are all delivered after the run
  thread exits (forwarder observes channel disconnect last).

### 8.3 Command mapping (`commands/agent_runs.rs`)

- Each §3.4 row: assert `ErrorKind` + curated message, and — for the
  credential case — that the message contains no secret substring.
- `start_agent_run` happy path: user message persisted before the thread
  starts; `{ run_id }` returned immediately; on simulated provider
  failure, no assistant message row exists and
  `agent_runs.error/final_content` match `RunFinished`.
- Provider/model not in the supported lists → `InvalidInput`.
- `resolve_agent_approval` with unknown `call_id` → `NotFound`.

### 8.4 conversation_id wiring

- `RunRecorder::with_conversation` → `read_run(id).conversation_id ==
  Some(id)`; delete-conversation cascade still removes runs+steps
  (extends `agent_runs.rs:519-542`).
- `list_runs_by_conversation` + `list_steps` power a full rehydration
  fixture: assert UI-shaped join (runs newest-first, steps seq ASC).

### 8.5 Accordion / hook (mock-driven, dev QA)

- `?mock` browser run: accordion appears under the user message, steps
  stream in by seq, approval block renders and auto-resolves, terminal
  pill matches `finished.status`, conversation switch away+back
  rehydrates the same run with identical step list.
- Plain-chat regression visual check: `send_message` flow renders
  exactly as before (no accordion, no event subscription side effects).
- `npx tsc` clean; `npx eslint .` clean; `cargo test`, `cargo clippy`
  (pedantic) clean; `npm run tauri dev` manual smoke of both paths.

---

## DECISION POINTS FOR HUMAN — RESOLVED (Task 5.1 kickoff)

The Human accepted the following decisions on 2026-08-29; they are **locked**
for the 5.1 implementation and must not be reopened:

| # | Decision | Ruling |
|---|---|---|
| 1 | Step events: extend `AgentRunEvent` emitted by the recorder (§1.1-1.2) vs separate side-channel | **ACCEPTED: recorder-emitted extension.** `seq` structurally aligned with `agent_steps.seq`; CF-01-safe (emit only after successful insert). |
| 2 | Live `ModelTurnStarted`/`ToolCallStarted` events in 5.1? | **ACCEPTED: completed-steps only.** No "started" events in 5.1. |
| 3 | Autonomy mode + approval resolvers for 5.1 | **ACCEPTED: gate ATTACHED, default `SemiAutonomous`;** minimal `resolve_agent_approval` + bare inline Approve/Deny; plus `extend_agent_run` + Continue for budget parks; **NO pause command** (Paused unreachable). |
| 4 | Concurrency | **ACCEPTED: one active run per conversation, parallel across conversations.** |
| 5 | `extend_agent_run` in 5.1? | **ACCEPTED: ships in 5.1.** |
| 6 | Accordion placement | **ACCEPTED: chronological placement inside the thread** (by `started_at`/`finished_at`). |
| 7 | Message persistence | **ACCEPTED: factor out shared helpers** — no duplicated insert logic between plain chat and agent runs. |
| 8 | Orphaned-`running` startup sweep | **DEFERRED TO 5.2 — do NOT implement in 5.1.** 5.1 renders such rows client-side as-is. |
| 9 | Registry cleanup on run-thread panic | **ACCEPTED: entry may leak only on run-thread panic** (runner is panic-free by design); 5.2 hardening. |

*No silent product choices remain: every section resolves to a concrete
variant, command, event name, or hook signature, and the nine points
above are the complete set, now resolved by the Human.*


