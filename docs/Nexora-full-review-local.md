# UNTRACKED DO NOT COMMIT — Nexora full-repo review (read-only, local)

- Date (UTC): 2026-09-08. Reviewer: Cline-equivalent executor, working tree only.
- Branch reviewed: `task/providers-wire-1.2.0` — **no branch switch performed**.
- This file is untracked by design. Do not `git add`, commit, push, PR, or tag it.
- No secrets in this report. No product code was edited for this review.

---

## 1. Snapshot

- **SHA:** `9d1a5cce0c06b5785347b4afc6e9cdb9a9048f52` — `chore(release): 1.2.0`
- **Versions (all three agree):** `package.json` 1.2.0 · `src-tauri/Cargo.toml` 1.2.0 · `src-tauri/tauri.conf.json` 1.2.0
- **Provider count:** 7 — `openai`, `anthropic`, `gemini` (native) + `xkiro`, `openrouter`, `nvidia`, `opencode_zen` (all four behind one shared `OpenAiExecutor`).
- **Dirty files at review time:** none tracked-modified; worktree has pre-existing **untracked-only** clutter (`NEXORA-TASK-*.md`, `docs/Nexora-providers-*-local.md`, `docs/Nexora-AI-Model-Catalog-August-2026.md`, `research-track-a/`, `research-track-b/`, `research-synthesis/`, `kimi-design-reference/`, `for human do not open tis/`, `AGENTS.md`, `.clinerules/`, `README.local.md`, etc.). No staged product diffs.
- **Test baseline (observed, `cargo test` in `src-tauri/`, finished in seconds — no hang):**
  `397 passed; 0 failed; 1 ignored`. No STOP needed; nothing hung past 2 min.

## 2. Architecture — what is sound vs what is lying

**Sound (verified in code):**

- Layering holds: `commands/` is thin IPC translation (`src-tauri/src/commands/settings.rs`, `conversations.rs`, `agent.rs`); business rules live in `application/` (`execution.rs`, `agent/runner.rs`, `service.rs`, `conversations.rs`); SQLite in `infrastructure/repository/`; HTTP in `infrastructure/providers/`; keys via `keyring` crate only.
- Provider-independent boundary is real: `ProviderExecutor` (`application/execution.rs:344`) sees only `AiRequest`/`AiResponse`/`ExecutorError`. All three native wires plus the four compat endpoints map into it; `ExecutorRegistry::new` (`execution.rs:377`) registers exactly the 7 names `supported_providers()` advertises — no shadow providers, no fallback to an arbitrary provider on unknown name (`resolve` returns `None`).
- Tools wire is genuinely native per provider, not faked: OpenAI `tools`/`tool_calls`/`role:tool` (`openai.rs:315`), Anthropic `input_schema`/`tool_use`/`tool_result` (`anthropic.rs:281`), Gemini `functionDeclarations`/`functionCall`/`functionResponse` + thought-signature round-trip (`gemini.rs:296,336`). Empty-tools omission is byte-compatible (`skip_serializing_if`, all three).
- Error bodies are never read — confirmed in all three `send()` fns (`openai.rs:467`, `anthropic.rs:498`, `gemini.rs:674`): non-2xx classifies by status + optional integer `Retry-After` only. Logs carry category only (`log::warn!("{name} request failed: {error}")`).
- Attachments: `file_path` never crosses the provider boundary (`AiAttachment` carries name/size/mime/payload only, `execution.rs:196`); export covers conversation+messages only, no attachments/paths/credentials (`application/export.rs:66,129`).
- Every Tauri command I checked is registered in `generate_handler!` (`lib.rs:28`): `send_message`, `supported_providers`, `get/set_setting`, `start_agent_run`, etc. No missing-registration time bomb found in the reviewed set.

**Lying / overclaiming (README/code/docs vs reality):**

1. `src-tauri/src/infrastructure/providers/mod.rs:113-118` (test comment) and `CHANGELOG.md` 1.2.0 both say the four 10-ID shortlists are "frozen from each provider's live `/models` catalog" as if that settled correctness. **A `/models` listing is not chat+tools capability.** The smoke file in this same worktree proves it: catalog-advertised `tools:true` / docs-chat-row IDs returned 403/402/404/401/400 on `POST chat/completions` (12 of 40 shortlist IDs dead on arrival — see §3).
2. `README.md:11` ("each with a maintained list of supported models") — the lists are hardcoded (true per DATABASE.md §7.5) but not currently maintained against reality: all four compat `models[0]` defaults fail live smoke (§3), and the three native defaults were never smoke-tested in any repo artifact I found.
3. `docs/Nexora-AI-Model-Catalog-August-2026.md` §2 states "Nexora implements exactly three AI providers" — stale since v1.1.0 (seven now). The doc is a dated research artifact; it should be marked superseded, not cited as current.
4. `docs/Nexora-providers-notes-local.md` Phase B claimed defaults "`[0]` are docs-grounded and tools-capable" — smoke falsified this for all four providers (xKiro `[0]` 403, OpenRouter `[0]` 402, NVIDIA `[0]` 404, Zen `[0]` 400).

## 3. Provider wire — defaults, smoke fate, chat vs agent risk

Smoke source: `docs/Nexora-providers-smoke-local.md` (80 calls, status-only, chat + agent-shaped-tools per ID). Rule used below: keep iff chat is 2xx; agent-usable iff tools is 2xx.

| Provider | Default (`models[0]`, what a fresh switch persists) | Smoke fate of default | Chat risk | Agent risk (agent always sends `tools`) |
|---|---|---|---|---|
| openai | `gpt-5.6-terra` | not in smoke set (native; catalog-doc live) | unknown-live | unknown-live |
| anthropic | `claude-sonnet-5` | not in smoke set (new ID, unverified) | unknown-live | unknown-live |
| gemini | `gemini-3.6-flash` | not in smoke set (new ID, unverified) | unknown-live | unknown-live |
| xkiro | `openai/gpt-5.6-sol` | **403/403 → drop** (entitlement; paid docs-default) | dead default | dead default |
| openrouter | `openai/gpt-5.2` | **402/402 → drop** (no credits; 402 blocks even `:free` IDs per OpenRouter docs) | dead default | dead default |
| nvidia | `nvidia/llama-3.1-nemotron-70b-instruct` | **404/404 → drop** | dead default | dead default |
| opencode_zen | `deepseek-v4-flash-free` | **400/400 → drop** (invalid) | dead default | dead default |

Notes on classification asymmetry (same wire event, different user message): OpenAI 404→`InvalidRequest`; Anthropic 404→catch-all `Failure`; Gemini 404→catch-all `Failure`; **402 has no arm anywhere** → catch-all `Failure` ("failed to fulfil", unactionable). OpenRouter's 402 means "top up credits" and even gates free models — the current text tells the user nothing.

**D8 keep-lists, pasted as recommendation (not applied — no code edits per task):**

- **xkiro keep (8, all 200/200):** `deepseek/deepseek-v4-flash` (suggested default — free, tools-ok), `qwen/qwen3.5-omni-plus:free`, `minimax/minimax-m3:free`, `minimax/minimax-m2.7:free`, `qwen/qwen3.5-plus:free`, `qwen/qwen3.6-plus:free`, `qwen/qwen3.7-plus:free`, `deepseek/deepseek-v4-pro`. Drop: `openai/gpt-5.6-sol`, `openai/gpt-5.3-codex-spark` (403/403).
- **openrouter keep (7 tools-ok + 1 chat-only):** `minimax/minimax-m3:free` (suggested default), `minimax/minimax-m2.7:free`, `inclusionai/ling-3.0-flash-fin:free`, `nvidia/nemotron-3.5-lightning:free`, `nvidia/nemotron-3-ultra-550b-a55b:free`, `nvidia/nemotron-3-super-120b-a12b:free`, `cohere/north-mini-code:free`, plus `z-ai/glm-5.2:free` (chat 200, tools 429 — keep, tools rate-limited). Drop: `openai/gpt-5.2` (402). Rate-limited, keep listed: `google/gemma-4-31b-it:free` (429/429).
- **nvidia keep:** `nvidia/nemotron-3-super-120b-a12b` (suggested default, 200/200), `nvidia/nemotron-3-ultra-550b-a55b` (200/200), `meta/llama-3.2-11b-vision-instruct` (200/200), `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` (tools 200, chat 503 — agent-ok, chat flaky), `openai/gpt-oss-20b` (chat 200, tools network — chat-only until tools retried). Drop: `nvidia/llama-3.1-nemotron-70b-instruct`, `moonshotai/kimi-k2.6`, `mistralai/mistral-large` (404/404). Investigate/retry: `mistralai/mistral-nemotron` (network/500), `moonshotai/kimi-k3` (network/network).
- **opencode_zen keep (3, all 200/200):** `ling-3.0-flash-fin-free` (suggested default), `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free`. Drop: `deepseek-v4-flash-free` (400), `deepseek-v4-flash`, `deepseek-v4-pro`, `minimax-m3`, `glm-5.2` (401). Rate-limited, keep listed: `big-pickle`, `mimo-v2.5-free` (429/429).

Net: after culling, the four compat shortlists shrink 40 → ~23 entries, every default becomes a smoke-2xx/2xx ID, and no `models[0]` is a paid/entitlement-gated ID.

## 4. Security — no leak found; one billing-adjacent hygiene note

- **Keyring:** credentials live only in the OS keyring (`keyring` crate), read at execution time (`RequestExecutionService::resolve_credential`, `execution.rs:481`), passed by reference into `execute`, dropped on return. Never persisted to SQLite (schema has no credential column; `data_management.rs:43` documents AC-10). No P0.
- **Logs/errors:** bodies never read; `Display` impls are fixed strings; `AgentError::Provider` reprints the already-classified `ExecutorError` text (`runner.rs:151`). Sentinel `sk-*` strings in tests are fake and asserted-absent from outputs (`agent.rs:396` sentinel test, `credential_never_appears_in_returned_error` in anthropic/gemini). Export contains no secrets, no file paths, no attachment bytes (§2). `clear_application_data` deliberately leaves keyring credentials intact and says so in the UI.
- **Thought-signature pass-through** (`ToolCall.thought_signature`, Gemini 3) is documented "never logged, never persisted, never parsed" (`execution.rs:79`) — I verified the type shape but did **not** fully audit `agent_steps` persistence columns for it; filed as P2 verification with a concrete command (§7).
- **Non-issue confirmed:** `#[allow(dead_code)]` on `OpenAiWireToolCall.type` (`openai.rs:448`) is serde-consumed wire compat, not hidden logic. No exfiltration-shaped code, no extra HTTP clients, no telemetry.

## 5. UX — Settings, errors, approvals

- **Provider switch auto-persists a dead default (the "just works" killer):** `SettingsView.handleProviderChange` writes `def.models[0]` (`SettingsView.tsx:71-83`), and all four compat `[0]`s are smoke-dead. A new user who picks xKiro/OpenRouter/NVIDIA/Zen gets a guaranteed failure on first send. Custom-ID support is otherwise genuinely good: backend union-validation + charset rule (`commands/settings.rs:87`), mirrored in UI (`isCustomModelId`, `useProviders.ts:35`), `Custom…` option not hidden (`SettingsView.tsx:283-310`), persisted custom IDs survive reload and provider switches.
- **Provider-switch keeps a custom ID across providers** (`useProviders.ts:140-146`, `SettingsView.tsx:73`) — provider-independent by design, but a bare xKiro-style ID 404s on providers needing prefixes, and the UI gives no hint. Minor.
- **Errors shown to the user are honest but uneven:** chat shows `error.message` and agent shows the classified `agentError` string (`ConversationView.tsx:349-359`); Settings surfaces `store.error.message`. The classified set (429+Retry-After, 5xx, network, 401/403, 400) is actionable — except the catch-all `Failure`, which is exactly what 402 and Anthropic/Gemini-404 currently produce.
- **Agent approvals:** autonomy ladder UI (Supervised/Semi-auto/Full-auto, `ConversationView.tsx:363`), per-call approve/deny/cancel/continue/pause/resume via `AgentRunSteps`; parked approvals never auto-resolve per README. No UX defect found here; approval-denied becomes a controlled observation and the loop continues (`runner.rs` Task 4.1 docs).
- **Spend-guard UX wrinkle:** the policy rate (5M/25M micro-USD per 1M tokens, `pricing.rs:19-22`) applies to **every** model including `:free`/`-free` IDs, so agent runs on free models burn (fake) budget and can trip `spend_limit_exceeded`. Users on free tiers will hit a money error for free calls.

## 6. Quality — tests, CI, flakiness

- **Tests:** 397 passed / 0 failed / 1 ignored, ~4 s. Coverage of the wire is genuinely good: per-provider contract serialization, tool round-trips, timeout threading, header placement (`x-api-key`, `anthropic-version`, `x-goog-api-key`, OpenRouter Referer/Title), status classification, secret-absence assertions, agent loop/governance/persistence/stress suites.
- **CI (`.github/workflows/ci.yml`):** ubuntu-only, 15-min timeout, `fmt --check` → `clippy -D warnings` → `cargo test`. Gaps: (a) this is a **Windows-primary** desktop app (agent system prompt is Windows-first, `cfg(windows)` prompt) with **no Windows CI job**; (b) no frontend gate at all (`tsc`, `eslint` unrun — AGENTS.md lists both as manual commands); (c) keyring-on-headless-Linux behavior is unexercised-by-design locally — if any future test touches `CredentialStore`, ubuntu headless (no dbus) is where it will hang/fail first.
- **Flaky/hang vectors (minor, none observed):** `request_timeout_is_threaded_through_send` sleeps 2 s vs 200 ms timeout (generous margin, fine); header-capture tests use 5 s `recv_timeout` (fine); test server threads `accept()` then `join()` with no timeout — a client that never connects would stall that test until the global CI timeout rather than fail fast. Pre-existing pattern, acceptable, noted for hygiene.
- **Deps:** minimal and appropriate (`Cargo.toml`): `reqwest` with `default-features=false + blocking/json/rustls` (no native-tls), `rusqlite` bundled, `keyring`, `base64`, two Tauri plugins (dialog, fs) — both with capability entries expected; nothing to cull.

## 7. Prioritized recommendations

### P0 (app does not "just work" until these land)

**P0-1 — Cull shortlists to smoke-2xx IDs.**
Problem: 12 of 40 compat shortlist IDs fail live (403×2 xKiro, 402 OpenRouter default, 404×3 NVIDIA, 400×1 + 401×4 Zen); every compat default is dead, so first-run on any compat provider fails.
Evidence: `openai.rs:81-137` (`XKIRO_MODELS`, `OPENROUTER_MODELS`, `NVIDIA_MODELS`, `OPENCODE_ZEN_MODELS`) vs `docs/Nexora-providers-smoke-local.md:17-96`.
Proposed change: replace the four arrays with the §3 keep-lists (~23 IDs); set defaults to `deepseek/deepseek-v4-flash` (xKiro), `minimax/minimax-m3:free` (OpenRouter), `nvidia/nemotron-3-super-120b-a12b` (NVIDIA), `ling-3.0-flash-fin-free` (Zen). Keep 429-only IDs listed (rate-limited ≠ dead). Effort: S.

**P0-2 — Default NVIDIA must not be the 70b ID (fold into P0-1, called out explicitly).**
Problem: `NVIDIA_MODELS[0]` = `nvidia/llama-3.1-nemotron-70b-instruct`, smoke 404/404; `SettingsView.handleProviderChange` auto-persists `[0]`, so selecting NVIDIA NIM is broken by construction.
Evidence: `openai.rs:111-112` + `SettingsView.tsx:80-82` + smoke lines 57-58.
Proposed change: default to a 200/200 nemotron-3 ID per §3. Effort: S (one-line once P0-1 lands).

**P0-3 — Do not treat `/models` as capability (process + comment fix).**
Problem: catalog listing ≠ chat+tools reachability; the repo's own comments/CHANGELOG assert otherwise, which will re-bite on the next "freeze from catalog" pass.
Evidence: `providers/mod.rs:113-118`, `CHANGELOG.md` 1.2.0 entry, notes-local Phase B claim vs smoke results.
Proposed change: amend the three comments to state shortlists are gated on live `POST chat/completions` chat+tools smoke (cite the smoke file); adopt the rule "keep iff chat 2xx; agent-usable iff tools 2xx; 429-only stays listed". Effort: S.

**P0-4 — Chat path has no request timeout (unbounded hang).**
Problem: `ConversationService::send_message` builds `AiRequest` with `request_timeout: None` (`application/conversations.rs:258`; same in `commands/conversations.rs:224,269`), while the agent runner always sets 120 s (`runner.rs:103,239,438`). A stalled chat POST hangs the `spawn_blocking` thread forever; UI shows "typing" indefinitely with no recovery except restart.
Evidence: files/lines above; executors honor `None` as unbounded by documented design (`openai.rs:466`).
Proposed change: set the same 120 s default on the chat path (single shared const), surfacing the existing `Network` ("could not be reached (network or timeout)") text. Effort: S.

### P1

**P1-1 — Map HTTP 402 (and Anthropic/Gemini 404) to actionable categories.**
Problem: OpenRouter 402 = "insufficient credits, top up — blocks even free models", but surfaces as opaque `Failure`; Anthropic/Gemini unknown-model 404 likewise.
Evidence: `classify_status` in `openai.rs:549`, `anthropic.rs:535`, `gemini.rs:721` (no 402 arm; 404→catch-all except OpenAI).
Proposed change: add a classified variant (e.g. `PaymentRequired` → "provider reported insufficient credits/quota (HTTP 402); top up or switch to a free-tier ID") and align Anthropic/Gemini 404 with OpenAI's `InvalidRequest`. Effort: S.

**P1-2 — Stop billing free-tier models at the paid policy rate.**
Problem: spend guard charges 5M/25M micro-USD per 1M tokens on `:free`/`-free` IDs; free-model agent runs can die with `spend_limit_exceeded`.
Evidence: `agent/pricing.rs:16-22` + `CHANGELOG.md` 1.1.0 ("Pricing unchanged").
Proposed change: $0 rate for known-free ID shapes (`:free` suffix, `-free` infix, Zen free list) or a per-provider free flag; keep conservative rate for everything else. Effort: S–M (needs a test table).

**P1-3 — Smoke-verify the three native defaults before calling lists "maintained".**
Problem: `gpt-5.6-terra`, `claude-sonnet-5`, `gemini-3.6-flash` appear in no smoke artifact; native chat could be as dead as the compat defaults were.
Evidence: absence in `docs/Nexora-providers-smoke-local.md`; consts at `openai.rs:64`, `anthropic.rs:87`, `gemini.rs:82`.
Proposed change: extend the existing status-only smoke script to the three native endpoints (same 60 s/timeout discipline, no bodies/keys recorded). Effort: S.

**P1-4 — CI does not match the product (no Windows job, no frontend gate).**
Problem: Windows-primary Tauri app gated only on ubuntu Rust; `tsc`/`eslint` never run in CI so frontend regressions land silently.
Evidence: `.github/workflows/ci.yml` (single `ubuntu-latest` job, three Rust steps).
Proposed change: add `windows-latest` Rust job + a lightweight `npx tsc` / `npx eslint .` job. Effort: M.

### P2

**P2-1 — Verify thought_signature never persists.**
Problem: documented pass-through-only, but not traced end-to-end in this review.
Evidence: `execution.rs:79-81` claim; persistence columns in `application/agent/persistence.rs` + `StepEventFrame` (`service.rs:89`).
Proposed change: run `rg -n "thought_signature" src-tauri/src/application/agent/persistence.rs src-tauri/src/application/agent/service.rs src-tauri/src/infrastructure/repository/agent_runs.rs` — expect hits only in runner mapping, none in recorder/INSERT paths; add an assertion if missing. Effort: S.

**P2-2 — Custom model ID kept across provider switches can 404 silently.**
Problem: a valid-custom ID for one provider (e.g. xKiro `vendor/model`) is kept when switching to another where it is invalid; failure arrives as a confusing provider error.
Evidence: `useProviders.ts:140-146`, `SettingsView.tsx:72-76`.
Proposed change: on provider switch, keep the custom ID but show a non-blocking hint ("custom ID, not verified for <provider>"), or re-validate on first send. Effort: S.

**P2-3 — Mark the August model-catalog doc superseded; stop hand-mirroring shortlists in the mock.**
Problem: catalog doc §2 ("exactly three providers") is stale and will mislead; `src/lib/mockBackend.ts:59-139` hand-copies the shortlists and will drift.
Evidence: catalog doc §2; `mockBackend.ts:59`.
Proposed change: add a superseded banner pointing at the smoke file + `SUPPORTED_MODELS`; generate or lint mock shortlists from one source. Effort: S.

**P2-4 — Test-server `join()` without timeout.**
Problem: `server.join().expect(...)` after every provider test stalls to the CI global timeout instead of failing fast if the client never connects.
Evidence: e.g. `openai.rs:874`, `anthropic.rs:893`, `gemini.rs:1058` pattern repeated ~15×.
Proposed change: leave as-is unless CI ever stalls there; if touched, wrap joins with a watchdog channel. Effort: S (deferred; no action now).

---

## Appendix — commands run (all read-only except writing this file)

- `git log -1 --pretty="%H %s"` → `9d1a5cc… chore(release): 1.2.0`; `git rev-parse --abbrev-ref HEAD` → `task/providers-wire-1.2.0`; `git status --short` → untracked-only (list in §1).
- `cargo test` in `src-tauri/` → 397 passed, 0 failed, 1 ignored (no hang; no STOP needed).
- Read Palm-to-root: `providers/{openai,anthropic,gemini}.rs` (wire, classify, tests), `providers/mod.rs`, `application/execution.rs`, `application/agent/{runner,service}.rs` (+ `pricing.rs`, `export.rs` excerpts), `commands/settings.rs`, `lib.rs` handler list, `application/conversations.rs` (send path), `src/lib/useProviders.ts`, `src/components/{SettingsView,ConversationView}.tsx`, `src/lib/mockBackend.ts` (provider lists), `docs/{ARCHITECTURE,DATABASE}.md` excerpts, `README.md`, `CHANGELOG.md`, `.github/workflows/ci.yml`, `src-tauri/Cargo.toml`, smoke + notes local docs.
- Not run (out of scope for a read-only pass): `npm run tauri dev`, `npx tsc`, `npx eslint .` — flagged in P1-4 instead.
