# M03 durable-cursor consumer verification

Functional history reconciliation; no visual redesign or new controls. Product source: ab92f9f8e9b7a31d8a6d05395b8ca1078f5dae5e. Visual QA: not design.

Full local gate passed once on final source: both-workspace fmt/clippy, 141 root Rust tests, 1261 desktop Rust tests (1 existing provider-gated test ignored), 1075 Vitest tests in 56 files, TypeScript, desktop cargo build and real embedded-frontend Tauri debug build. Targeted hook tests: 33; hook/component/barrel combined: 64.

The hook consumes the existing conversation-scoped forward journal in bounded pages, advances processed next_cursor rather than notification high-water, and reconciles manual reload/subscription gaps without restoring executable approvals or dispatching tools. Generation/request fences cover A-B-A switches, overlap, lazy steps, observations, late terminal pages and unmount. Legacy run metadata stays bounded and old steps remain lazy. The dev mock explicitly returns an empty journal for pre-journal fixtures.

## Windows native baseline and restart

Built executable SHA-256: ca3afe9ee89d223bb43c452d0fc9c75aac9f660bcb21708a4aeda18eb0b1a58c. Started with child-only synthetic APPDATA/LOCALAPPDATA and explicit WebView2 profile, using a synthetic database and workspace. No provider connection, agent run or tool was submitted.

The original binary and final binary both displayed retained completed run 92001 and its original read_file step. After final-build restart the same history rendered and Send remained disabled without a configured provider. Original run/step/journal rows match the pre-start snapshot exactly (1/1/3), schema v16, integrity ok, no foreign-key violations or running rows. The known QA process stopped with orderly shutdown and zero outstanding run/task handles. See startup-result.json and synthetic seed.sql. Window-detection samples are receipts, not controlled performance measurements.

Concurrency and deliberate delivery-loss cases were exercised in deterministic hook tests, not manually with a real provider. Windows native startup/history was observed; native macOS/Linux UI smoke remains part of later platform acceptance. No three-platform release claim is made here.
