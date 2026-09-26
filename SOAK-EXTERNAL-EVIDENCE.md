# soak-external: Repeat Bead 1 Gates Twice on Clean Worktree

Bead: `627649ee-e646-4a2a-aaed-dd99e5f4a25e`
Branch: `gt/shadow/627649ee`
Base commit: `bed0c5a` (Merge pull request #58 from sshdw/task/exhausted-recovery-variant)
Worktree state: clean before and after both runs (`git status --porcelain` empty).

No production code, no timing tests, and no test files were modified. `node_modules/` was
installed to make the TypeScript gate executable; it is git-ignored and produced no
tracked-file diff.

## Environment

| Item                        | Value                              |
| --------------------------- | ---------------------------------- |
| Node                        | v24.21.0                           |
| TypeScript (via `npx tsc`)  | 5.9.3                              |
| npm install                 | 177 packages, 9.2s, exit 0         |
| CPUs                        | 4                                  |
| Memory                      | 12220 MB total, ~3579 MB available |
| `cargo`                     | **not installed**                  |
| `rustc`                     | **not installed**                  |
| `rustup`                    | **not installed**                  |
| `pkg-config webkit2gtk-4.1` | **no**                             |
| `rust-toolchain.toml` pins  | channel `1.97.1`                   |

## Evidence Table (two runs of the Bead 1 gates)

| Gate                                        | Run 1                    | Run 1 dur | Run 2                    | Run 2 dur | Variance                 | Delta                         |
| ------------------------------------------- | ------------------------ | --------- | ------------------------ | --------- | ------------------------ | ----------------------------- |
| `cargo fmt --check`                         | BLOCKED (exit 127)       | 6 ms      | BLOCKED (exit 127)       | 8 ms      | none (identical failure) | +2 ms noise                   |
| `cargo clippy --all-targets -- -D warnings` | BLOCKED (exit 127)       | 10 ms     | BLOCKED (exit 127)       | 9 ms      | none (identical failure) | -1 ms noise                   |
| `cargo test`                                | BLOCKED (exit 127)       | 6 ms      | BLOCKED (exit 127)       | 5 ms      | none (identical failure) | -1 ms noise                   |
| `npx tsc`                                   | PASS (exit 0, no output) | 15057 ms  | PASS (exit 0, no output) | 17412 ms  | 2355 ms (+15.6%)         | within noise for a 4-core box |

Exit code 127 = `cargo: command not found` from the shell, in all six cargo invocations
across both runs. This is an environment/toolchain gap, not a code failure and not a
flake: the failure is byte-identical in run 1 and run 2.

## Additional tsc repetitions (flakiness check)

Because only one gate was executable, `npx tsc` was repeated two more times:

| Run | Result | Duration |
| --- | ------ | -------- |
| 1   | PASS   | 15057 ms |
| 2   | PASS   | 17412 ms |
| 3   | PASS   | 20791 ms |
| 4   | PASS   | 13559 ms |

4/4 pass, zero diagnostics emitted, zero output diff between runs. Duration spread
13559-20791 ms (mean 16705 ms, -34%/+24% around the mean) is attributable to shared-CPU
contention on a 4-core box with ~3.5 GB available memory; it does not affect the
pass/fail result.

## Differences Between Runs

- Pass/fail: identical across both runs for all four gates.
- Diagnostics emitted: none in any run, for any gate.
- Timing: only variance observed. No gate flipped state between runs.
- No flaky or non-deterministic behavior observed in the executable gate.

## Blocker on the three Rust gates

The three cargo gates could not be exercised at all in this container:

1. No Rust toolchain is present and none is on `PATH`
   (`cargo`, `rustc`, `rustup` all absent; no `~/.cargo`, `/usr/local/cargo`, or
   `/root/.rustup`).
2. Even with a toolchain installed, `src-tauri/Cargo.toml` depends on `tauri = "2"`
   with default features, which on Linux pulls `wry` -> `webkit2gtk-sys` and requires
   `webkit2gtk-4.1` via `pkg-config`. `pkg-config --exists webkit2gtk-4.1` returns false
   and `ldconfig -p` lists zero `webkit2gtk` libraries, so `cargo test` would fail at
   dependency build time.
3. `rusqlite` is configured with `features = ["bundled"]`, requiring a C compiler for
   libsqlite3 in addition to the above.

Installing a Rust toolchain and GTK/WebKit development packages would mean modifying the
container environment, which is out of scope for this bead. The Rust gates therefore
remain unverified rather than failing: no claim about the health of
`cargo fmt`/`clippy`/`test` on this commit can be made from this run.

## Reproduction

```
cd src-tauri && cargo fmt --check                                   # requires toolchain
cd src-tauri && cargo clippy --all-targets -- -D warnings           # requires toolchain + webkit2gtk-4.1
cd src-tauri && cargo test                                          # requires toolchain + webkit2gtk-4.1
npx tsc                                                             # requires npm install
```
