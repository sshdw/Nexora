# M03 durable-cursor consumer verification

Functional history reconciliation; no visual redesign or new controls. Final product source: 2445283a28a35fd785fe8d4157782c324e0751e1. Visual QA: not design.

All ten local gate stages passed on final source: both-workspace fmt/clippy, 141 root Rust tests, 1261 desktop Rust tests (1 existing provider-gated ignored), 1080 Vitest tests in 56 files, TypeScript, desktop cargo build and real embedded-frontend Tauri debug build. Targeted hook tests: 38; touched-file ESLint and Prettier passed.

The hook consumes the existing conversation-scoped forward journal in bounded pages, advances processed next_cursor rather than notification high-water, and reconciles manual reload/subscription gaps without restoring executable approvals or dispatching tools. Generation/request fences cover A-B-A switches, overlapping reloads, lazy steps, observations, late terminal pages and unmount. Legacy run metadata stays bounded and untouched terminal history stays lazy. A terminal run partially materialized by late live delivery recovers its missing persisted steps through the existing fenced lazy reader. The dev mock returns an empty journal for pre-journal fixtures.

Regression coverage includes a pending initial page with partial live completion, twelve-step history followed by delayed live delivery after cursor acknowledgement (with and without Finished), split-page interleaving, failed-read/manual-reload recovery, and newer-terminal fences. The three late-delivery orderings reproduced missing history before the repair and pass afterward.

## Windows native baseline and restart

Built executable SHA-256: da133eb3e714d82d7472b7ea48356f83a318a4653e6be3afe834237e124d9c7e. Started with child-only synthetic APPDATA/LOCALAPPDATA and explicit WebView2 profile, using a synthetic database and workspace. No provider connection, agent run or tool was submitted.

Original and final binaries displayed retained completed run 92001 and its original read_file step. After final-build restart the same history rendered and Send remained disabled without a configured provider. Original run/step/journal rows match the pre-start snapshot exactly (1/1/3), schema v16, integrity ok, no foreign-key violations or running rows. The known QA process stopped with orderly shutdown and zero outstanding run/task handles. See startup-result.json and synthetic seed.sql. Window-detection samples are receipts, not controlled performance measurements.

Concurrency and deliberate delivery-loss cases were exercised in deterministic hook tests, not manually with a real provider. Windows native startup/history was observed; native macOS/Linux UI smoke remains part of later platform acceptance. No three-platform release claim is made here.
