# M03 durable-cursor consumer verification

Functional history reconciliation; no visual redesign or new controls. Product source: b6ec19ddf7e7e3789b008b7afb6bd0a6a2d8cf5a. Visual QA: not design.

Full local gate passed once on final source: both-workspace fmt/clippy, 141 root Rust tests, 1261 desktop Rust tests (1 existing provider-gated test ignored), 1076 Vitest tests in 56 files, TypeScript, desktop cargo build and real embedded-frontend Tauri debug build. Targeted hook tests: 34.

The hook consumes the existing conversation-scoped forward journal in bounded pages, advances processed next_cursor rather than notification high-water, and reconciles manual reload/subscription gaps without restoring executable approvals or dispatching tools. Generation/request fences cover A-B-A switches, overlap, lazy steps, observations, late terminal pages and unmount. Legacy run metadata stays bounded and old steps remain lazy. The dev mock explicitly returns an empty journal for pre-journal fixtures.

## Windows native baseline and restart

Built executable SHA-256: a03a6a4cd617ff7bd086928845bd88dcb2b51649ab0aaee8a1830d7b25942e23. Started with child-only synthetic APPDATA/LOCALAPPDATA and explicit WebView2 profile, using a synthetic database and workspace. No provider connection, agent run or tool was submitted.

The original binary and final binary both displayed retained completed run 92001 and its original read_file step. After final-build restart the same history rendered and Send remained disabled without a configured provider. Original run/step/journal rows match the pre-start snapshot exactly (1/1/3), schema v16, integrity ok, no foreign-key violations or running rows. The known QA process stopped with orderly shutdown and zero outstanding run/task handles. See startup-result.json and synthetic seed.sql. Window-detection samples are receipts, not controlled performance measurements.

Concurrency and deliberate delivery-loss cases were exercised in deterministic hook tests, not manually with a real provider. Windows native startup/history was observed; native macOS/Linux UI smoke remains part of later platform acceptance. No three-platform release claim is made here.

## Concrete review repair

Independent review found that an initial cursor page could omit an earlier committed step after a partial live list received Finished. Round 2 restricts lazy initial-history skipping to wholly unloaded terminal lists and merges missing steps into partial lists. The exact deferred regression failed with [2] before the fix and passes with [1, 2]. Full gate and native smoke were repeated after this source edit; no optional cleanup was added.
