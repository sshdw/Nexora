# M03.a Windows runtime verification

Backend-only phase: no visual redesign or new UI behavior. This receipt distinguishes native runtime observations from automated API tests. The full M03 milestone is not closed by this increment.

## Build and isolation

- Product source/test head: `a0f963dcb2c9d00648ef976f2ae8c6215c33cf9d`.
- Real Tauri debug build with embedded production frontend, not a dev/mock backend.
- Executable SHA-256: `0aa04f04c49e63c31b1ec0697837705e1ea024149298d9a462f1bd2bf7799ab5`.
- Child-only APPDATA points to a synthetic QA profile. No personal database or provider credential was used; no agent request or tool was submitted.
- Local gate: fmt/clippy with warnings denied in both workspaces; root Rust 139 passed; desktop Rust 1257 passed/1 existing real-provider test ignored; Vitest 1052 passed (56 files); TypeScript, cargo build and real Tauri build passed. Existing linker library-creation and frontend bundle-size advisories are recorded in build logs, not compiler/clippy gate failures.

## Native observations

1. Start the built app on the isolated populated v15 P01 fixture (7 runs/14 steps). The production startup migrates to v16. Full original run/step rows remain byte-for-value unchanged; zero durable events are backfilled; FK check is clean. See `upgrade-result.json`.
2. In the actual native window, the retained Russian interrupted history renders. Expanding the existing synthetic read-file step reveals its original arguments and `Synthetic retained file body`. This is a history smoke, not aesthetic approval.
3. Close the known QA process completely. Apply `journal-startup-seed.sql` ONLY to that isolated v16 DB. It inserts a synthetic linked active run/step, with triggers producing run_started and step_recorded. This bypasses the start IPC deliberately; it does not establish provider dispatch or live notification behavior.
4. Restart the actual app. Its lease-protected orphan sweep records exactly one run_interrupted event, retains the original step identity/spend and exposes raw error/interrupted=1 with one step.
5. Close and restart again. All three durable events and terminal fields stay identical, all original legacy rows stay unchanged, there are zero active runs and FK check is clean. See `restart-result.json`. The last known QA process is stopped.

## Automated checks and limits

Shared-core/application tests cover ordered cursor round-trip and bounded reads, stable identities, duplicate/retry behavior, concurrent ordering, rollback, preservation, cascade privacy, checkpoint failure before later dispatch, and missed/duplicate notification reconciliation by API identity. The registered IPC command and typed wrapper are covered by command/parity tests and static production-entrypoint checks. These are distinct from the native startup observations above; a live provider run and direct interactive cursor-IPC exercise were not performed.

Existing UI hydration remains unchanged. The additive cursor API does not yet make the frontend automatically heal missed live frames. The pre-existing foreign live-step scoping issue is outside M03.a and remains a companion M03 task. No questions, queues, Plan, executable approval restoration, tool replay or manual Continue is added.

Windows native startup/history smoke is complete. Linux/Windows CI must pass on the final PR head. Native macOS/Linux startup evidence is still unavailable here and remains explicit M03/platform acceptance work; do not infer a three-platform release pass.
