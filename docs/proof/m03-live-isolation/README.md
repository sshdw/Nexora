# M03.b live-event conversation isolation

Functional IPC/hook correction; no visual changes or schema migration. Backend Visual QA: not design. Current layout, controls and recorded-history presentation are preserved.

## Verification

- Source head: `77af6b0aa19e29fea8e2a018ef2500ace0ba896f`.
- Sol medium implemented; owner-requested Sol high finisher required no further edits. Independent review is a separate gate.
- Final local gate: root Rust 141 passed; desktop Rust 1257 passed/1 existing provider-gated ignored; Vitest 1055 passed (56 files); TypeScript, fmt/clippy with warnings denied in both workspaces, cargo build and real Tauri build PASS.
- Targeted behavior: hook 13 passed, service 30 passed. Tests reject foreign/missing-identity step and governance frames before mutation/fetch, retain owned early events before the start response, reject delayed foreign events after switching, preserve existing duplicate-step and governor behavior, and check serialized production-forwarder identities.
- Real embedded-frontend Tauri executable SHA-256: `89257141aa58a3829659b0ad43d9e55d27857eddf49a9c33f07f91356b4f2a37`.
- Started the native app with child-only APPDATA in a synthetic v16 profile. The native WebView exposed retained interrupted run 91007 and its recorded step labels. All 8 run rows, 15 step rows and 3 journal records match the pre-start snapshot; zero active runs and FK check clean. See startup-result.json. Known QA process stopped afterward.
- No provider or tool request was submitted. Actual interleaved live provider sessions were not manually driven; deterministic hook/service tests establish the routing behavior separately from the native startup smoke.

The default build still reports the existing linker-library and frontend chunk-size advisories; compiler/clippy warning gates pass. Native Linux/macOS UI smoke remains a platform acceptance gap; Linux/Windows CI must be green before merge.

No durable cursor consumer, polling, reconnect redesign, executable approval restoration, replay, resume, new controls or visual refinement is included.
