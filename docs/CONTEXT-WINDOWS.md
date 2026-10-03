# Nexora Context Windows and Capability Fallback Policy

This note documents the enforced context-window policy and the
capability-gated fallback behavior: why a model may get a smaller window
or fewer capabilities than its provider's marketing claims.

Code is the source of truth. The research-only model catalog
(`docs/Nexora-AI-Model-Catalog-*`) may claim larger per-model windows;
no per-model bump is applied without live verification.

## 1. Enforced context windows

`context_limit_for(provider, model)` resolves the documented window in
this order: per-model map first, then provider fallback, then a
`gemini`-substring rule, then the default.

| Models | Enforced window |
| --- | --- |
| OpenAI: `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.6-sol` | 128,000 tokens |
| Anthropic: `claude-sonnet-5`, `claude-haiku-4-5-20251001`, `claude-opus-4-8` | 200,000 tokens |
| Gemini (verified): `gemini-3.6-flash`, `gemini-3.1-pro-preview`, `gemini-pro-latest` | 1,048,576 tokens |
| Gemini lite / unverified: `gemini-3.1-flash-lite`, `gemini-flash-lite-latest`, `gemini-3.5-flash`, `gemini-3.5-flash-lite` | 128,000 tokens (conservative baseline) |
| Any other provider (`openai`, `anthropic`, `gemini` fallback) | Provider window above |
| Unknown provider / unknown model | 128,000 tokens (default) |

A smaller (earlier-compacting) window is the safe direction for the
proactive trigger, so unverified windows are clamped down, never up.

### Lite clamp rationale

The lite/unverified Gemini IDs resolve to `GEMINI_LITE_CONTEXT_LIMIT`
(128,000) instead of the full 1M window until live-verified against a
real API. Raise a lite ID to its verified window only after live
verification with real credentials — never from marketing claims or the
research-only catalog. The section budgets derive from the same
resolution function, so a lite-clamped model automatically gets
proportionally smaller budgets.

### Effective window

An explicit per-request override wins when set (`Some(0)` means
unbounded passthrough); otherwise the effective window is the canonical
value above for that provider/model pair.

## 2. Proactive compaction

- Trigger: estimated input exceeds **0.8** of the usable window
  (`COMPACTION_THRESHOLD`), strict-`>` (exactly 0.8 stays quiet).
- Usable window = context limit minus **20,000** reserved output tokens
  (`COMPACTION_RESERVED_TOKENS`). A `0` limit means unbounded/dormant:
  no compaction.
- The assembly hook fires the existing compaction path exactly once; a
  still-overflowing second crossing surfaces the terminal
  `AgentError::ContextExhausted` variant — distinct from the retryable
  provider-side `ExecutorError::ContextLengthExceeded`, which triggers
  one in-place compaction and a resend instead of terminating.

## 3. Section budgets

The assembled input window is partitioned as fixed shares of the usable
window (integer floor; shares sum to 100%):

| Section | Share |
| --- | --- |
| System identity (pinned, never omitted) | 10% |
| Task + role (droppable) | 15% |
| Stage context (pinned while attached) | 15% |
| Tool outputs (newest-N, oldest-first drop) | 35% |
| History summary (lowest priority) | 25% |

Overflow trims lowest-priority first and oldest-first within a section;
system identity and the current stage truncate only as a last resort, so
the fitted context always fits the model limit.

## 4. Capability gating

- Listed models (members of the hardcoded `SUPPORTED_MODELS` lists) are
  tool/vision/JSON capable; unlisted IDs get conservative `false` flags.
- `recommended_models(provider, require_tools)`: when tools are required,
  non-tool models are filtered out. Unknown providers yield an empty
  list.
- There is deliberately **no cross-provider fallback**: a terminal
  failure or a skipped model for one provider never tries another
  provider's models.

## 5. Compat (user-configured OpenAI-compatible endpoint) fallback

- The `openai_compat` provider carries **no hardcoded model list by
  design** (empty list): the model identifier comes from the endpoint
  configuration, and the UI accepts the configured ID for it.
- `supports_tools` is a user toggle on the endpoint config (default
  `true`). Capability-gated tool calling: an endpoint without
  function-calling support never receives the `tools` member (graceful
  degrade). A misconfigured endpoint fails closed before any network
  activity.
- Custom (unlisted) model IDs **pass through for chat** (`require_tools
  == false`) on any build-supported provider — the only resolution path
  for providers with no hardcoded list — but are **skipped fail-closed
  for agent runs** (`require_tools == true`): unknown capability never
  auto-runs tools. Skipped entries continue down the routing profile in
  order; resolution never substitutes another provider's models.

## 6. Pricing-estimate disclaimer

All cost figures are **hardcoded estimates, not live pricing**: known-free
IDs bill $0, listed native IDs bill their table estimate, and everything
else bills a single conservative policy default. Rates are never fetched
over the network. Treat every displayed cost as an approximation for the
spend guard and dashboard, not a bill.
