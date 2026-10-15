## What

## Why (issue link)

Closes #

## Gate (run in order, all green)

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test` (inside `src-tauri/`)
- [ ] `npx tsc`
- [ ] New Tauri command? Registered in `generate_handler!`, typed wrapper in `src/lib/tauri.ts`, permission entries if it's a new plugin API

## Visual QA (UI PRs only)

- [ ] Real Tauri build, before/after shots, journey script run
- [ ] Reduced-motion path verified where motion was touched
