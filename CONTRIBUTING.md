# Contributing to Nexora

## Setup

- Node + the Rust toolchain pinned by `src-tauri/rust-toolchain.toml`
  (rustup picks it up automatically).
- Run: `npm run tauri dev` (Vite uses port 1420, `strictPort` — free it first).
- Build: `npm run tauri build`.

## Before you push — the gate, in this order

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test` (inside `src-tauri/`; single test: `cargo test <name>`)
4. `npx tsc`

Frontend tests are vitest (`npm test`); all Rust tests are `#[test]`s against
in-memory SQLite under `src-tauri/src/`.

## Adding a Tauri command — all three steps, no exceptions

1. `#[tauri::command]` fn in `src-tauri/src/commands/<area>.rs`
   (thin translation only, no business logic, no repository access).
2. Register it in the `generate_handler![...]` list in `src-tauri/src/lib.rs`
   (missed registration compiles but fails at invoke time).
3. Add a typed wrapper in `src/lib/tauri.ts` (`invoke` args are camelCase,
   Rust params snake_case; timestamps are Unix seconds).

New plugin APIs also need permission entries in
`src-tauri/capabilities/default.json`. Errors become secret-free
`CommandError` — no credentials, SQL, or message content in error text.

## Commits and PRs

- Conventional commits with scope: `feat(providers): …`, `fix(ui): …`.
- One logical change per PR; PRs merge only on green CI, sequentially.
- UI PRs: real-Tauri-build before/after shots, no new UI dependency without
  a freeze note, reduced-motion path verified where motion was touched.
