# NIGHT-SCOPE 2.0 — Draft (docs only, no implementation)

> Branch: `task/night-scope` (from updated `origin/main`). This file is the ONLY change.
> Gate: none (docs only).

## Inputs (accepted as verified, spot-checked file:line only)

- **Regression all-green:** `ContextExhausted` distinct + `terminal_outcome` error mapping
  (`application/agent/persistence.rs:75`, `errors.rs:35,59,77`); `with_context_limit` wired
  (`runner.rs:313`, `service.rs:574`) + dormant-at-0. No fixes needed.
- **Docs-truth:** catalog 1.05M/1M vs code 128k/200k intentional conservative (accepted);
  unverified gemini-lites at full 1M window (generous-direction risk);
  `anthropic.rs:91` comment claims 1M vs enforced 200k; `mockBackend` 128k dev-only (ignore);
  compat providers fallback 128k undocumented.
- **Sweeps:** clippy default+pedantic clean; one cleanup (`runner.rs:1562` `run_with_gate` → `#[cfg(test)]`); secrets none.
- **Team audit H-items (verification required before fix, NOT fixes now):**
  H-1 `write_file` jail escape (`executor.rs:1129-1144,349-362`);
  H-2 deny bypass `edit_file`/`search_files` (`permissions.rs:322-327`, `dispatch.rs:216`);
  H-6 unpinned Rust tree (no `Cargo.lock`); H-4 dead `workspace_root`/governance;
  M-1 spend-guard pricing placeholders; M-2 context zeros.

## Scope

- Verification passes only (reproduce-or-dismiss) for each audit H/M item above.
- Live-verification of gemini-lite context windows (no code change until measured).
- Pinning decision for the Rust tree (`Cargo.lock` in or out, with rationale).
- One micro-batch: fix `anthropic.rs:91` 1M comment + gate `run_with_gate` to `#[cfg(test)]`.
- Documentation of intentional conservative windows (catalog vs code) and compat fallback.

## Non-goals

- No implementation, refactoring, or behavior change in this draft.
- No fixes to H-1/H-2/H-4/H-6/M-1/M-2 — verification passes only.
- No provider model-list changes, no pricing-table changes, no migration changes.
- No `NEXORA-*.md` files created or modified; no spec (`docs/`) edits.
- No toolchain, CI, or capability changes.

## Risks

- Gemini-lite full-1M window is unverified and generous-direction: over-admission risks
  mid-run truncation / spend before the live check lands.
- Catalog-vs-code gap (1.05M/1M vs 128k/200k) is accepted as conservative, but any future
  "alignment" in the wrong direction silently expands spend/exhaustion surface.
- `anthropic.rs:91` 1M comment vs 200k enforcement misleads future contributors.
- H-1/H-2, if confirmed, are sandbox/permission escapes — verification harness itself must
  stay read-only and non-destructive.
- No `Cargo.lock` means non-reproducible Rust builds; pinning decision has supply-chain weight.
- Dead `workspace_root`/governance (H-4) risks scope confusion if fixed without spec approval.

## Proposed 2.0 tasks (exactly 8)

### 1. Audit H-item verification passes — effort: medium
Reproduce-or-dismiss each: H-1 jail escape (`executor.rs:1129-1144,349-362`),
H-2 deny bypass (`permissions.rs:322-327`, `dispatch.rs:216`), H-4 dead
`workspace_root`/governance, M-1 spend-guard placeholders, M-2 context zeros.
Read-only harnesses, per-item verdict (confirmed / dismissed with evidence). No fixes.

### 2. Gemini-lite window live-verification — effort: low
Live-measure actual usable windows for gemini-lite entries currently admitted at full 1M.
Record results; propose clamp only if measured < admitted. No model-list edit here.

### 3. Cargo.lock pinning decision — effort: low
Decide: commit `Cargo.lock` (reproducible app builds) vs stay unpinned, with written
rationale and CI implication. Decision record only; the actual pin/unpin lands in 2.0.

### 4. Micro-batch: anthropic comment + `run_with_gate` cfg — effort: minimal
Correct `anthropic.rs:91` 1M comment to enforced 200k (or cite source), and move
`runner.rs:1562` `run_with_gate` under `#[cfg(test)]`. Smallest reviewable diff; no logic change.

### 5. Conservative-window doc note (catalog vs code) — effort: minimal
Record the accepted 1.05M/1M-vs-128k/200k conservative stance and why "aligning up"
is forbidden without live-verification + spend review. Prevents generous-direction drift.

### 6. Compat-provider 128k fallback disclosure — effort: minimal
Document the undocumented 128k fallback for compat providers (xKiro/OpenRouter/NVIDIA NIM/
OpenCode Zen path): where it applies, why, and what a future change requires. No code change.

### 7. Context-zero dormant-at-0 semantics note — effort: low
Pin the verified dormant-at-0 behavior of `with_context_limit` (M-2 context): definition,
covering test, and what "0 means dormant, not unlimited" implies for future callers. Guards
against zero-means-unlimited regressions.

### 8. Spend-guard placeholder inventory — effort: medium
Inventory M-1 pricing placeholders: which provider/model entries are estimates, exposure if
wrong, and what live pricing source resolves each. Inventory only; table edits are 2.0 work.
