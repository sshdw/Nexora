# P01 real-build proof

Captured by the orchestrator on 2026-11-09 from the actual non-headless Tauri Windows app, embedded frontend at http://tauri.localhost/. Source commit 555ab07; no mock query/backend or provider request. Fixed window capture 1536x816 throughout. PNGs are unmodified returned main-window captures; tiny transient cursor surfaces were not used as proof frames.

Executable SHA-256: `012e27d89a8e995b1d21af747abfa478e9487b34d8a7e1347b3e971130601643`.

## Isolation and data

The child process alone received a new APPDATA under the repository local `.ai/validation/p01/appdata`; the personal database was not used. Initialize an empty v15 app DB, close that known QA process, apply `seed.sql`, and restart the same binary/profile. A later closed-app seed `single-run-seed.sql` adds an isolated single-run conversation. Both fixtures are synthetic. No tool/model execution was requested.

Native startup observation: original orphan 91001 becomes raw error/interrupted=1 with 2 steps; same-text legacy error 91002 stays raw error/interrupted=0. Original 6 runs/12 steps/spend 900 and their terminal fields remain unchanged across the next restart. Isolated orphan 91007 also becomes interrupted with 2 steps. Total 7 runs/14 steps, no running rows, no extra steps, foreign-key check clean. These backend observations are distinct from what screenshots alone prove.

## Frame index

| Frame | MD5 | Colors | What I saw |
|---|---|---:|---|
| 01-interrupted-vs-legacy-error.png | `323ac7578b56e382646256c0a212079f` | 24092 | English mixed history: legacy error 91002 retains failure/retry copy; interrupted91001 has quiet status, retained-history copy and expanded synthetic tool result. Other historical budget/spend states are outside the target cards. |
| 02-interrupted-only-en.png | `e3a18fe10793588f4c9c0abb88074e2e` | 20639 | English isolated interrupted91007: retained steps/output expanded; composer has disabled Send, not Stop; no Continue/retry/approval control for this run. |
| 03-interrupted-only-ru.png | `d3438c8932490c7a266ba1e20a4e2cee` | 22617 | Russian isolated interrupted91007: localized status and retained-history copy, expanded synthetic result; no Continue/retry/Stop control. |
| 04-no-active-tasks-ru.png | `c09c107d525a5d359087d522ceb93b18` | 23069 | Russian interrupted history with Activity popover reporting no active work; retained synthetic output remains visible. |

## Validation

Independent code review: MERGE. Final local gate: root Rust 131 passed; desktop Rust 1251 passed/1 existing env-gated real-provider smoke ignored; TypeScript PASS; Vitest 1051 passed; fmt/clippy PASS for both workspaces. Real Tauri debug build with embedded production frontend PASS. One Windows linker library-creation message is a build warning; clippy with -D warnings passed.

This is P01 history/status evidence, not UI redesign, provider execution, manual continuation, or three-platform release validation. Other seeded budget states intentionally retain their existing Continue affordance; scope no-Continue assertions to marked interrupted runs.

## Patched SQLite verification

After prerequisite PR #321 merged, P01 was rebased without conflicts. Frame 05 was captured from source commit `84535ed` using the same isolated child APPDATA and the embedded production frontend. Executable SHA-256: `bf0dd2918af88109fbc12ba6918156954aeb3142d3a3c8ba650cb7b808552747`.

| Frame | MD5 | Colors | What I saw |
|---|---|---:|---|
| 05-patched-sqlite-restart-ru.png | `33e526c7e1fb91f5f8f68b9cde183a1a` | 22356 | Russian interrupted run 91007 retains its expanded synthetic tool result; Activity says no work is running; disabled Send and no Continue/retry control are visible. |

Read-only native database comparison before/after this startup found exactly the same 7 runs, 14 steps, status markers, terminal timestamps, spend and step contents; foreign-key check stayed clean. No input was submitted to the agent. The frame does not establish the SQLite engine version; the dependency lockfiles and linked-runtime regression guard cover that separately.

Final gate after rebase: root Rust 132 passed; desktop Rust 1251 passed/1 existing provider-gated test ignored; TypeScript PASS; Vitest 1051 passed across 56 files; fmt and clippy with warnings denied PASS in both workspaces. Real Tauri build PASS. The known Windows linker library-creation warning remains. This supplemental capture changes no application code.
