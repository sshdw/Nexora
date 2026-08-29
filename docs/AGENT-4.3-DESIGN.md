# Agent Run Financial Spend Guard - Design (Task 4.3.0)

Status: **implemented — Task 4.3 backend (metering, pricing, limit, persistence) shipped; this document is the design record.**
Scope: backend metering + pricing + enforcement + persistence + events. UI
(E5 / 5.1 / 5.2) is out - it consumes the new events/columns later.

This document reverses the Task 1.2 decision to defer usage parsing: 4.3
meters every provider turn, prices it in integer micro-USD, stops the ReAct
loop once a configured per-run limit is exceeded, persists the outcome when a
recorder is attached, and emits a classified event - all strictly opt-in and
backward-compatible. The step governor (`AgentError::BudgetExhausted(usize)`
and status `budget_exhausted`, `runner.rs:114`, `persistence.rs:300`) is
**untouched**: financial spend and step allowance are disjoint concepts and
stay that way.

---

## 1. Metering - usage across the provider-independent boundary

### 1.1 Boundary extension (suggested shape)

`AiResponse` (`execution.rs:200-208`) currently carries `content`, `model`,
`tool_calls` only. Add one field:

```rust
/// Token usage for one provider response (Task 4.3). `None` is legal and
/// means "usage was absent from the provider response" - streaming and
/// usage-less responses must stay valid for every executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct TokenUsage {
    /// Input (prompt) tokens billed for this turn.
    pub input_tokens: u64,
    /// Output (completion) tokens billed for this turn.
    pub output_tokens: u64,
}
```

`AiResponse` gains `pub usage: Option<TokenUsage>` (`execution.rs:201`).
`Option` is mandatory: the three executors may legally omit usage, and every
existing test fixture stays a valid `AiResponse` by setting `usage: None`.

### 1.2 Provider parse rules

Each provider parses its own wire `usage` into `TokenUsage`; `None` when the
upstream response omits the block. All wire additions are `Option` +
`#[serde(default)]` so an absent or partial block never fails the request.

| Provider | Upstream shape (HARD FACT) | Wire struct addition | Mapping |
|---|---|---|---|
| OpenAI | `usage.{prompt_tokens, completion_tokens}` | `ChatCompletionResponse.usage: Option<OpenAiUsage>` (`openai.rs:253`) | `input_tokens = prompt_tokens`, `output_tokens = completion_tokens`; `Some(TokenUsage{..})` when present, else `None` (`openai.rs:319-346` `to_ai_response`) |
| Anthropic | `usage.{input_tokens, output_tokens}` | `AnthropicResponse.usage: Option<AnthropicUsage>` (`anthropic.rs:335`) | direct; `None` when absent (`anthropic.rs:368-408`) |
| Gemini | `usageMetadata.{promptTokenCount, candidatesTokenCount}` | `GenerateContentResponse.usage_metadata: Option<GeminiUsage>` with `#[serde(rename = "usageMetadata")]` (`gemini.rs:336`) | `input_tokens = promptTokenCount`, `output_tokens = candidatesTokenCount`; `None` when absent (`gemini.rs:395-440`) |

Unknown/incomplete sub-fields inside a present block (e.g. OpenAI omits a token
count) map that sub-count to `0` - conservative, never fabricate a positive
number. A present-but-empty `usage` object still yields `Some(TokenUsage{0,0})`
and costs $0.

### 1.3 Failure posture when usage is absent

Absent usage is **not** an error. It produces `AiResponse { usage: None, .. }`;
the request completes exactly as before (`execution.rs:244-256`
`ProviderExecutor::execute` returns `Ok`). Spend accounting then treats the
turn as $0 (see section 7.1).

### 1.4 Impact on every `AiResponse` constructor

Production constructors (add a `usage` field):

- `openai.rs:341-345` (`to_ai_response`) - `usage: mapped`.
- `anthropic.rs:403-407` (`to_ai_response`) - `usage: mapped`.
- `gemini.rs:433-439` (`to_ai_response`) - `usage: mapped`.

Test fakes / literals (all gain `usage: None`, mechanical only):

- `runner.rs` helper `text_response` (`runner.rs:593-599`) and `tool_step`
  (`runner.rs:1037-1043`), plus ~20 inline `AiResponse { .. }` literals in the
  runner test module (e.g. `runner.rs:686`, `runner.rs:730`, `runner.rs:783`,
  `runner.rs:809`, `runner.rs:858`, `runner.rs:913`, `runner.rs:1254`,
  `runner.rs:1265`, `runner.rs:1388`, `runner.rs:1435`, `runner.rs:1477`,
  `runner.rs:1516`, `runner.rs:1550`, `runner.rs:1568`, `runner.rs:1592`,
  `runner.rs:1641`, `runner.rs:1678`, `runner.rs:1711`, `runner.rs:1720`,
  `runner.rs:1769`, `runner.rs:1894`, `runner.rs:1935`).
- `conversations.rs` test fixtures (e.g. `conversations.rs:941-945`; ~18 inline
  `AiResponse` literals in its `#[cfg(test)]` module).
- Provider unit tests that build their **wire** response structs with struct
  literals (`ChatCompletionResponse` in `openai.rs` tests; `AnthropicResponse`
  in `anthropic.rs` tests; `GenerateContentResponse` in `gemini.rs` tests) gain
  the new `usage` field (`None`) - a struct literal cannot fall back to a serde
  default.

New metering tests should exercise the wire parse through `serde_json::from_str`
(JSON fixtures) rather than struct literals, so they assert the real upstream
shapes.

### 1.5 Serialization note (additive)

`AiResponse` derives `Serialize` (`execution.rs:200`) and is returned by the
`send_message` command (`commands/conversations.rs:119-127`), crossing to the
frontend. Adding `usage: Option<TokenUsage>` is additive: it serializes as
`"usage": null` (no usage) or an object - never removing an existing key. The
frontend type in `src/lib/tauri.ts` may later add an optional `usage` field;
that is 5.1/5.2 work and is explicitly out of scope here.

---

## 2. Pricing - source of truth, units, staleness

### 2.1 Recommendation: code constants per model, USD per 1M tokens

New module `src-tauri/src/application/agent/pricing.rs` (application layer -
pricing is a business rule, not provider-wire detail). One table, one "as of"
date, one lookup:

```rust
pub(crate) struct ModelPrice {
    /// USD per 1M input tokens, in micro-USD (1 USD = 1_000_000 micro-USD).
    pub input_micro_usd_per_1m: u64,
    /// USD per 1M output tokens, in micro-USD.
    pub output_micro_usd_per_1m: u64,
}

/// `Some(ModelPrice)` for a known model id, `None` for an unknown id.
pub(crate) fn model_price(model: &str) -> Option<ModelPrice>;
/// Known price, else the conservative fallback (section 2.3). Never `None`.
pub(crate) fn price_for(model: &str) -> ModelPrice;
```

Policy table (single conservative default applied to every model;
see `src-tauri/src/application/agent/pricing.rs`):

| scope | input per 1M | output per 1M | micro input | micro output |
|---|---|---|---|---|
| **policy default (all models)** | $5.00 | $25.00 | 5_000_000 | 25_000_000 |

All model ids - including Gemini and any future id - resolve to this one
policy rate. No per-model hardcoding is done; the table is a deliberate
placeholder pending the Phase 5 settings surface (see pricing module header).
Unknown models use the same default, so FR-004 pass-through is never broken
(Decision Point 3).

### 2.2 Units: integer micro-USD `u64`

- No floats: `f64` USD has representation error, and enforcement hinges on an
  exact `spent > limit` comparison that must be deterministic (the loop's
  determinism invariant, `runner.rs:277-283`).
- 1 USD = 1_000_000 micro-USD keeps every price in the table an exact integer
  (the cheapest Luna price, $0.20/1M = 200_000 micro, is still integral).
- Token counts are integers; cost = price x tokens / 1_000_000 is pure integer
  arithmetic.

### 2.3 Unknown model: conservative fallback (not hard-unknown)

`price_for` returns the single policy default for any id:
`POLICY_DEFAULT_INPUT_MICRO_PER_1M = 5_000_000` ($5),
`POLICY_DEFAULT_OUTPUT_MICRO_PER_1M = 25_000_000` ($25).

Why fallback over hard-unknown (which could only mean "fail the run"):
SRS FR-004 requires the model id to pass through never-rejected/never-
substituted (`openai.rs:53-69`; the same pass-through contract holds for
Anthropic/Gemini). A hard-unknown that *errors* the run would break the "no
limit -> zero change" doctrine every time a user selects a model not in the
price table. A finite, conservative fallback keeps every run viable while
erring toward over-accounting for unknown models.

### 2.4 Staleness / "as of" policy

The policy is documented in `pricing.rs`:

```rust
/// Policy placeholder pending Phase 5 settings. Conservative default:
/// 5_000_000 micro-USD input / 25_000_000 micro-USD output per 1M tokens.
/// Adjust here when the settings surface lands; see DATABASE.md Tç7.8.
pub(crate) const POLICY_DEFAULT_INPUT_MICRO_PER_1M: u64 = 5_000_000;
pub(crate) const POLICY_DEFAULT_OUTPUT_MICRO_PER_1M: u64 = 25_000_000;
```

The existing `SUPPORTED_MODELS` consts (`openai.rs:63`, `anthropic.rs:86`,
`gemini.rs:81`) are now documented as pricing-agnostic (see
`crate::application::agent::pricing`); no per-model price row is required.

---

## 3. Limit source - per-run configuration

### 3.1 Recommendation

A per-run field on `AgentRunner`, not DB settings in 4.3 (settings/UI later):

```rust
// runner.rs - new field on AgentRunner
spend_limit_micro_usd: Option<u64>,

// runner.rs - new #[must_use] builder
pub(crate) fn with_spend_limit(micro_usd: u64) -> Self { /* sets Some(micro_usd) */ }
```

`new()` (`runner.rs:192-203`) initializes it to `None`. `None` = no financial
guard = the loop keeps the exact pre-4.3 behaviour (the opt-in doctrine already
enforced for `control`, `approval_gate`, `recorder`, `runner.rs:168-186`).

### 3.2 Naming + units decision

Recommend `with_spend_limit(micro_usd: u64)` with the unit in the parameter
name, **not** the task's suggested literal `with_spend_limit_usd`. The `_usd`
suffix contradicts an integer micro-USD argument and invites an off-by-1e6
caller bug. (See Decision Point 7.)

Limit semantics: `Some(0)` is legal and stops after the first turn that has any
positive cost; `None` is "unlimited".

---

## 4. Enforcement semantics - stop on exceed

### 4.1 Check placement: after each model turn

You cannot refuse before knowing usage, so the check runs **after**
`executor.execute` returns and `steps_taken` is incremented. Exact anchor
(`react_loop`, `runner.rs:368-390`): insert accumulate + enforce between the
existing `check_cancellation(control)?` (line 379) and the
`if response.tool_calls.is_empty()` branch (line 381), so a very expensive
**final** turn still trips the guard before returning `Ok`.

Ordering rationale: the existing `check_cancellation` (line 379) runs first, so
a user cancellation wins over a spend trip in a race - both are terminal, and
cancellation (governance) takes precedence.

### 4.2 Accumulation + enforcement (pseudo-signature)

```rust
// react_loop gains a &mut u64 accumulator, 0-initialized in run():
fn react_loop(&self, ..., record: Option<&mut ActiveRunRecord<'_>>,
              spend_micro_usd: &mut u64) -> Result<String, AgentError> {
    ...
    let response = self.executor.execute(&request, credential)?;
    steps_taken += 1;
    // (existing model_turn record, runner.rs:371-377)
    ...
    // NEW: accumulate + enforce only when a consumer exists:
    if let Some(usage) = response.usage {
        if let Some(limit) = self.spend_limit_micro_usd {
            let cost = pricing::turn_cost_micro_usd(usage, pricing::price_for(model));
            *spend_micro_usd = spend_micro_usd.saturating_add(cost);
            if *spend_micro_usd > limit {
                self.emit(AgentRunEvent::SpendLimitExceeded {
                    spent_micro: *spend_micro_usd,
                    limit_micro: limit,
                });
                return Err(AgentError::SpendLimitExceeded {
                    spent_micro: *spend_micro_usd,
                    limit_micro: limit,
                });
            }
        } else if record.is_some() {
            let cost = pricing::turn_cost_micro_usd(usage, pricing::price_for(model));
            *spend_micro_usd = spend_micro_usd.saturating_add(cost);
        }
    }
    if response.tool_calls.is_empty() { ... }
}
```

`turn_cost_micro_usd` - ceiling integer division, 128-bit intermediate to
avoid overflow, saturate to `u64`:

```rust
fn turn_cost_micro_usd(usage: TokenUsage, price: ModelPrice) -> u64 {
    let in_cost = ((usage.input_tokens as u128) * (price.input_micro_usd_per_1m as u128))
        .div_ceil(1_000_000u128).min(u64::MAX as u128) as u64;
    let out_cost = ((usage.output_tokens as u128) * (price.output_micro_usd_per_1m as u128))
        .div_ceil(1_000_000u128).min(u64::MAX as u128) as u64;
    in_cost.saturating_add(out_cost)
}
```

### 4.3 New classified error (disjoint from step budget)

`AgentError` (`runner.rs:107-120`) gains a variant; `Display` (`runner.rs:122-136`)
and `source` (`runner.rs:138-145`) gain the matching arm;
`From<ExecutorError>` (`runner.rs:147-151`) is unaffected.
`BudgetExhausted(usize)` stays exactly as-is.

```rust
SpendLimitExceeded { spent_micro: u64, limit_micro: u64 },
// Display: "agent stopped: spend limit exceeded (spent $X.YYYYYY of $W.ZZZZZZ)"
// source(): None
```

USD formatting from micro-USD is `format!("{}.{:06}", v / 1_000_000, v % 1_000_000)`.

### 4.4 New event

`AgentRunEvent` (`control.rs:79-111`) gains:

```rust
SpendLimitExceeded { spent_micro: u64, limit_micro: u64 },
```

Emitted once at the trip, best-effort via the existing `emit`
(`runner.rs:472-476`). `BudgetExhausted { max_steps }` (`control.rs:86-89`) is
untouched.

### 4.5 New terminal run status + migration

Recommend a **new** status `spend_limit_exceeded` (see Decision Points 1/2 for
the evaluated alternative `error`). Rationale: a spend trip is a governance
guardrail (like `budget_exhausted`/`cancelled`), not a classified failure, and
5.1/5.2 must filter it independently without string-matching against `error`
text (`DATABASE.md:283` documents `error` as "classified error text").

This widens the section 7.8 status CHECK (`DATABASE.md:278`,
`database.rs:272`), which requires migration v5 (next section). The persistence
`finalize` match (`persistence.rs:290-310`) gains:

```rust
Err(AgentError::SpendLimitExceeded { .. }) => ("spend_limit_exceeded", None, None),
```

so `error` stays `NULL` for this terminal state, exactly like
`budget_exhausted`/`cancelled`.

---

## 5. Persistence - v5 migration, columns, no-recorder path

### 5.1 Columns on `agent_runs`

Add two terminal columns:

| Field | Type | Nullable | Default | CHECK | Notes |
|---|---|---|---|---|---|
| `spent_micro_usd` | INTEGER | NO | 0 | `spent_micro_usd >= 0` | Total billed micro-USD at finalize; 0 for unrecorded/unknown-usage turns |
| `limit_micro_usd` | INTEGER | YES | NULL | `limit_micro_usd IS NULL OR limit_micro_usd >= 0` | Per-run limit; NULL when no limit configured |

### 5.2 v5 migration SQL (single rebuild of `agent_runs`)

SQLite cannot `ALTER` a CHECK, so widening the status enumeration forces a
table rebuild (DATABASE.md Tç5 details the runner change that makes this safe).
Both the new status and the two new columns land in one v5 rebuild:

```sql
-- v5 (inside the migration transaction; foreign_keys already OFF at the
-- connection level by the runner wrapper, DATABASE.md Tç5):
CREATE TABLE agent_runs_new (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    conversation_id INTEGER
        CHECK (conversation_id IS NULL OR conversation_id > 0)
        REFERENCES conversations (id) ON DELETE CASCADE,
    model TEXT NOT NULL CHECK (length(model) > 0),
    mode TEXT NOT NULL
        CHECK (mode IN ('supervised', 'semi_autonomous', 'full_autonomous')),
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'completed', 'cancelled', 'budget_exhausted',
                          'spend_limit_exceeded', 'error')),
    started_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (started_at > 0),
    finished_at INTEGER CHECK (finished_at IS NULL OR finished_at > 0),
    total_steps INTEGER NOT NULL DEFAULT 0 CHECK (total_steps >= 0),
    final_content TEXT,
    error TEXT,
    spent_micro_usd INTEGER NOT NULL DEFAULT 0 CHECK (spent_micro_usd >= 0),
    limit_micro_usd INTEGER CHECK (limit_micro_usd IS NULL OR limit_micro_usd >= 0)
);

INSERT INTO agent_runs_new
    (id, conversation_id, model, mode, status, started_at, finished_at,
     total_steps, final_content, error)
    SELECT id, conversation_id, model, mode, status, started_at, finished_at,
           total_steps, final_content, error
    FROM agent_runs;

DROP TABLE agent_runs;
ALTER TABLE agent_runs_new RENAME TO agent_runs;

CREATE INDEX idx_agent_runs_conversation ON agent_runs (conversation_id);
CREATE INDEX idx_agent_runs_started ON agent_runs (started_at);
```

Existing rows migrate intact with `spent_micro_usd = 0`, `limit_micro_usd = NULL`
(the new columns are absent from the `INSERT` list so they take defaults - no
data is fabricated for pre-4.3 runs).

### Runner change required for v5 (documented, small)

`apply_migration` opens a transaction (`database.rs:371-382`) and `configure`
enables `PRAGMA foreign_keys=ON` (`database.rs:322-329`). `PRAGMA foreign_keys`
cannot be toggled *inside* a transaction, and `DROP TABLE agent_runs` (a
parent of `agent_steps.run_id`) must not run with FK enforced. Therefore:

- `MIGRATIONS` (`database.rs:97`) grows a per-entry flag, e.g.
  `(i64, &str, MigrationKind)` where
  `MigrationKind = Plain | RebuildParent`.
- `migrate()` (`database.rs:333-363`) wraps a `RebuildParent` entry with
  `conn.execute_batch("PRAGMA foreign_keys=OFF;")` **before**
  `apply_migration` and restores `PRAGMA foreign_keys=ON;` immediately after
  (on both success and error paths). This is the only production-file
  behavioural change beyond the agent/pricing code.

Fallback (no runner change): choose `error` instead of `spend_limit_exceeded`
(Decision Point 1) and v5 collapses to two pure `ALTER TABLE agent_runs ADD
COLUMN ...` statements - no rebuild, no FK toggle.

### 5.4 Repository + recorder threading (design-level)

- `AgentRun` (`agent_runs.rs:40-63`) gains `spent_micro_usd: u64` and
  `limit_micro_usd: Option<u64>`; the three `SELECT` column lists
  (`agent_runs.rs:188`, `agent_runs.rs:204`, `agent_runs.rs:224`) and
  `row_to_agent_run` (`agent_runs.rs:339-352`) extend accordingly.
- `create_run` (`agent_runs.rs:133-145`) is unchanged - the defaults cover the
  new columns.
- `finalize_run` (`agent_runs.rs:161-176`) gains
  `spent_micro_usd: u64, limit_micro_usd: Option<u64>` and the `UPDATE` sets the
  two columns at termination (terminal-fields-only, consistent with D12).
- `RunRecorder::finalize_run` (`persistence.rs:140-155`) and
  `ActiveRunRecord::finalize` (`persistence.rs:290-310`) thread the same two
  values; `AgentRunner::run` (`runner.rs:299-322`) initializes
  `spent_micro_usd = 0`, passes `&mut` through `react_loop`, and supplies it
  (plus `self.spend_limit_micro_usd`) to `finalize`.

### 5.5 No-recorder path

With no recorder attached nothing is written (pre-4.2 behaviour,
`persistence.rs:1-8`), and the financial guard still works: spend is
accumulated in memory and the trip still emits
`AgentRunEvent::SpendLimitExceeded` and returns
`AgentError::SpendLimitExceeded`. Spend lives **only** in memory and events
when unrecorded - never persisted.

### 5.6 DATABASE.md section 7.8 rows (proposed diff sketch)

Add after the `error` row:

| Field | Purpose | Type | Nullable | Default | CHECK | FK | UNIQUE | Traceability |
|---|---|---|---|---|---|---|---|---|
| spent_micro_usd | Total billed micro-USD at finalize | INTEGER | NO | 0 | `spent_micro_usd >= 0` | None | No | Task 4.3 |
| limit_micro_usd | Per-run spend limit in micro-USD | INTEGER | YES | NULL | `limit_micro_usd IS NULL OR limit_micro_usd >= 0` | None | No | Task 4.3 |

Amend the `status` CHECK cell to:
`status IN ('running', 'completed', 'cancelled', 'budget_exhausted', 'spend_limit_exceeded', 'error')`.

---

## 6. Backward-compatibility proof and test list

### 6.1 Every behaviour that must not change

1. **Plain (non-agent) chat** - `ConversationService::send_message` and the three
   executors return `Ok(AiResponse { usage, .. })`; the extra field is ignored
   by every non-agent consumer and serializes as `null` for usage-less fixtures.
2. **Agent without a limit** (`spend_limit_micro_usd == None`) - the react_loop
   performs no enforce/accumulate when `record` is also `None`; same answers,
   same step counting. Only the unavoidable wire-level `usage` parsing is added.
3. **Tool-only responses** - unchanged; `usage` may be `Some` but is ignored on
   the no-limit/no-recorder path.
4. **Error classification** - `Provider`/`BudgetExhausted`/`EmptyResponse`/
   `Cancelled` variants, their `Display`/`source`, and `From<ExecutorError>`
   are untouched; the new `SpendLimitExceeded` variant is additive.
5. **Status values and CHECK** - the existing five statuses remain valid; the
   v5 rebuild preserves every existing row's statuses and ids.
6. **Events** - every existing `AgentRunEvent` variant keeps its exact shape.
7. **Existing tests** - all `AiResponse` fixtures become `usage: None`
   (mechanical recompile only, no assertion changes).

### 6.2 Tests added in 4.3 (implementation)

- **pricing**: known model -> exact micro-USD; unknown model -> fallback;
  ceiling rounding at a sub-1M-token remainder; saturating overflow safety.
- **metering (per provider)**: OpenAI usage present/absent; Anthropic
  input/output; Gemini `usageMetadata` camelCase present/absent; present block
  with a missing token sub-field -> 0.
- **runner enforcement**: limit set, scripted usages exceed ->
  `SpendLimitExceeded { spent, limit }` + one `SpendLimitExceeded` event, no
  further tool dispatch; under limit -> completes with no event;
  exactly-at-limit -> completes; one-over -> trips; `usage: None` turns
  contribute 0 and do not trip; `no limit` -> byte-identical to pre-4.3;
  cancellation still wins over a spend trip.
- **recorder**: recorder+limit trip finalizes `spend_limit_exceeded` with both
  columns; recorder without limit finalizes `completed` with `spent>0`,
  `limit=NULL`; no recorder -> zero rows.
- **schema**: v5 accepts `spend_limit_exceeded`, rejects `warp`, `spent`
  default 0, `limit` NULLable and `>= 0`; rebuild preserves run/step/
  conversation FK cascades and pre-existing rows.

---

## 7. Risks and edge cases

1. **Usage absent mid-run** - recommend **count-as-known ($0)**. Fabricating a
   conservative estimate would invent spend the user never incurred and could
   spuriously stop runs; non-streaming providers reliably return usage, so the
   gap is rare and self-corrects on the next turn (Decision Point 4).
2. **Limit reached exactly at boundary** - use strict `spent > limit` ("exceed",
   not "reached"). A turn landing exactly on `limit` completes; the turn that
   pushes past it trips, and its `spent_micro` is the over-limit value in the
   event/error (Decision Point 7).
3. **Multi-tool-call turns** - usage is per *turn*, not per tool call: a
   response dispatching 3 tool calls is one `usage` entry counted once. There is
   no per-call pricing and none is introduced.
4. **Cancelled runs** - cancellation is checked before the spend check, so a
   cancel wins a race; a spend trip after a cancel loses. Both are terminal
   with their own status/error/event. A cancelled run with a recorder still
   writes its columns at finalize (whatever accumulated), status `cancelled`.
5. **Pricing unknown model** - conservative fallback (Opus 4.8, $5/$25 per 1M),
   logged at warn once per run; never fails the run.
6. **Overflow** - 128-bit cost intermediate + `saturating_add`/`.min(u64::MAX)`
   make the meter infallible; a saturated `spent` still trips an ordinary
   limit.
7. **Gemini prices unverified** - Gemini models resolve to the fallback until a
   price source is added via Phase 5 settings (Decision Point 3); this understates
   accuracy but overstates cost, which is safe for a stop-guard.

---

## 8. Decision points for Human

| # | Question | Recommendation | One-line rationale |
|---|---|---|---|
| DP-1 | Spend trip as new terminal status `spend_limit_exceeded` vs reuse `error`? | **New status.** | Guardrail is not a failure, and 5.1/5.2 filtering must not string-match error text. |
| DP-2 | Widen the status CHECK via an `agent_runs` rebuild (needs the DATABASE.md Tç5 FK-off runner tweak) vs keep change additive with `error`? | **Rebuild + tiny runner tweak.** | One tested forward-only migration; the alternative forfeits DP-1's clean filtering. |
| DP-3 | Gemini pricing (policy default). | **Every model uses the single policy default until Phase 5 settings.** | Never invent per-model rates; over-counting is safe for a stop-guard. |
| DP-4 | Usage absent mid-run: count-as-known $0 vs conservative estimate? | **Count-as-known $0.** | Fabrication could stop runs on spend the user never incurred. |
| DP-5 | Token->USD rounding: ceiling vs floor. | **Ceiling.** | A guardrail should over-account, never under-account. |
| DP-6 | Fallback price for unknown models. | **Max of supported set: $5 in / $25 out per 1M (5M/25M micro).** | Conservative, finite, keeps FR-004 pass-through intact. |
| DP-7 | Limit boundary: trip on `spent > limit` vs `spent >= limit`. | **`>` (exceed only).** | Name is "exceed"; "reached exactly" is not an exceed. |

---

## 9. Anchors verified (file:line)

- `src-tauri/src/application/agent/runner.rs` - `AgentError` `runner.rs:107-120`; `Display`/`source` `runner.rs:122-145`; `AgentRunner` struct `runner.rs:166-187`; `new` `runner.rs:192-203`; builder methods `runner.rs:205-266`; `run` `runner.rs:299-322`; `react_loop` `runner.rs:328-469`; post-execute/turn `runner.rs:367-381`; `emit` `runner.rs:472-476`; `honor_allowance` `runner.rs:514-539`; `text_response` `runner.rs:593-599`; `tool_step` `runner.rs:1037-1043`.
- `src-tauri/src/application/agent/control.rs` - `AgentRunEvent` `control.rs:79-111`; `BudgetExhausted { max_steps }` `control.rs:86-89`.
- `src-tauri/src/application/agent/persistence.rs` - opt-in doc `persistence.rs:1-8`; `finalize_run` wrapper `persistence.rs:140-155`; `ActiveRunRecord::finalize` `persistence.rs:290-310`; `DEFAULT_RECORDED_MODE` `persistence.rs:50`.
- `src-tauri/src/application/execution.rs` - `AiResponse` `execution.rs:200-208`; `Serialize` derive `execution.rs:200`; `ProviderExecutor` `execution.rs:244-256`.
- `src-tauri/src/infrastructure/repository/agent_runs.rs` - `AgentRun` `agent_runs.rs:40-63`; `create_run` `agent_runs.rs:133-145`; `finalize_run` `agent_runs.rs:161-176`; SELECT lists `agent_runs.rs:188`/`agent_runs.rs:204`/`agent_runs.rs:224`; `row_to_agent_run` `agent_runs.rs:339-352`.
- `src-tauri/src/infrastructure/database.rs` - `MIGRATIONS` `database.rs:97-307`; v4 block `database.rs:262-305`; status CHECK `database.rs:272`; `configure` (FK pragma) `database.rs:322-329`; `migrate` `database.rs:333-363`; `apply_migration` `database.rs:371-382`.
- Providers - `openai.rs:253` `ChatCompletionResponse`; `openai.rs:319-346` `to_ai_response`; `anthropic.rs:335` `AnthropicResponse`; `anthropic.rs:368-408` `to_ai_response`; `gemini.rs:336` `GenerateContentResponse`; `gemini.rs:395-440` `to_ai_response`.
- `src-tauri/src/commands/conversations.rs` - `send_message` returns `AiResponse` `commands/conversations.rs:119-127`.
- `src-tauri/src/application/conversations.rs` - test `AiResponse` fixtures `conversations.rs:941-945` (module pattern).
- `docs/DATABASE.md` - section 7.8 `DATABASE.md:258-286`; status CHECK `DATABASE.md:278`; `error` doc `DATABASE.md:283`.
- `src-tauri/src/application/agent/pricing.rs` - policy default 5_000_000/25_000_000 micro-USD per 1M (Task 4.3, Phase 5 placeholder).

---

## 10. Out of scope (unchanged)

No edits to `execution.rs`, providers, `runner.rs`, `database.rs`, IPC, UI, or
settings tables are made by this design; no real migration is applied; no push
or PR occurs. This document is the only deliverable.
