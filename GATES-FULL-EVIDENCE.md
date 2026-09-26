# gates-full: Clean Baseline at main@bed0c5a

**Bead:** `gates-full` (e88f83e4)
**Convoy:** night-overnight-2026-09-27 (4e677e71)
**Baseline commit:** `bed0c5a0d3547b40389162e069ad8a568297aa3c` (main HEAD)
**Worktree branch:** `convoy/night-overnight-2026-09-27/4e677e71/gt/toast/e88f83e4`
**Branch diff vs main:** _empty_ (`git diff main...HEAD` produces no output) — the convoy introduces **zero** code changes at this baseline.

## 1. Environment

The Rust crate graph lives in `src-tauri/` (the repo root has no `Cargo.toml`; this is a Tauri desktop app).

| Tool                                                           | Version                                    | Notes                                                                                                                                                                                   |
| -------------------------------------------------------------- | ------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| cargo / rustc                                                  | 1.97.1 (`1.97.1-x86_64-unknown-linux-gnu`) | Installed via rustup but **not on the default PATH**; made usable by adding `~/.cargo/bin` to PATH.                                                                                     |
| rustfmt / clippy                                               | 1.9.0 / 0.1.97                             | Initially **missing** from the toolchain; added with `rustup component add rustfmt clippy` (required so `cargo fmt`/`cargo clippy` execute).                                            |
| node                                                           | v24.21.0 / npm 11.19.0                     | Present.                                                                                                                                                                                |
| typescript (tsc)                                               | 5.9.3                                      | **Not installed locally** initially; `npx tsc` failed with "This is not the tsc command you are looking for." Installed via `npm install --no-save typescript` so the command executes. |
| pkg-config                                                     | present                                    | Resolves system C libraries for `-sys` crates.                                                                                                                                          |
| libglib2.0-dev / libgdk-pixbuf-0.48-dev / libgtk-3-0 (runtime) | present                                    | `glib-2.0`, `gobject-2.0`, `gio-2.0`, `gdk-pixbuf-2.0` pkg-config files resolve.                                                                                                        |
| libgtk-3-dev / libwebkit2gtk-4.x-dev                           | **missing**                                | `gdk-3.0.pc` and `webkit2gtk-4.0/4.1.pc` are absent. The account is `agent` (uid 1001) with **no sudo** and no `apt` write access, so these cannot be installed.                        |

## 2. Gate results (run exactly as specified)

Commands run from `src-tauri/` (Rust) and repo root (TypeScript). Full logs captured to `logs/fmt.log`, `logs/clippy.log`, `logs/test.log`, `logs/tsc.log` (these paths are gitignored).

| #   | Command                                     | Result | Exit | Duration                       | Blocking cause                                               |
| --- | ------------------------------------------- | ------ | ---- | ------------------------------ | ------------------------------------------------------------ |
| 1   | `cargo fmt --check`                         | PASS   | 0    | ~1.0s                          | —                                                            |
| 2   | `cargo clippy --all-targets -- -D warnings` | FAIL   | 101  | ~42.5s (cold) / ~1.3s (cached) | Dependency **fails to compile**; never reaches clippy lints. |
| 3   | `cargo test`                                | FAIL   | 101  | ~49.0s (cold) / ~6.0s (cached) | Same dependency compile blocker; no tests executed.          |
| 4   | `npx tsc`                                   | PASS   | 0    | ~8.3s                          | —                                                            |

## 3. Root cause of the cargo gate failures

`cargo clippy` and `cargo test` fail while **building third-party dependencies**, before any project or lint code is reached. The first crate to fail is `gdk-sys v0.18.2`:

```
error: failed to run custom build command for `gdk-sys v0.18.2`
  > PKG_CONFIG_ALLOW_SYSTEM_CFLAGS=1 pkg-config --libs --cflags gdk-3.0 'gdk-3.0 >= 3.22'

  pkg-config output:
    Package gdk-3.0 was not found in the pkg-config search path.
    Package 'gdk-3.0', required by 'virtual:world', not found

  The system library `gdk-3.0` required by crate `gdk-sys` was not found.
  The file `gdk-3.0.pc` needs to be installed ...
```

The dependency chain (from `cargo tree -i gdk-sys`) that pulls Windows-less Linux webview system libraries:

```
tauri v2.12.0
└── tauri-runtime-wry v2.12.0
    └── wry v0.57.0
        └── webkit2gtk v2.0.2
            └── gtk v0.18.2
                ├── gdk-sys v0.18.2  ← needs system gdk-3.0  (FAILS HERE)
                ├── gtk-sys v0.18.2  ← needs system gtk+-3.0
                └── ...
tao v0.37.1
└── gdkwayland-sys / gdkx11-sys v0.18.2  ← more GTK/Wayland system libs
```

Tauri 2 embeds a Linux webview (`wry`) that requires the GTK3 + WebKit2GTK system development packages. The container ships only the **runtime** `libgtk-3-0` and glib/gdk-pixbuf `-dev` packages; the matching `-dev` packages (`libgtk-3-dev` providing `gdk-3.0.pc`, and `libwebkit2gtk-4.0-dev`/`-4.1-dev`) are absent and **cannot be installed** because the process runs as a non-root user with no `sudo`/write access to `apt`.

> Note on precision: this is a **system C-library / pkg-config** shortage for the Tauri Linux webview stack — _not_ a missing Rust toolchain (the toolchain is installed via rustup) and _not_ a clippy lint violation. `gdk-sys` is the first sys crate in the build graph to fail; resolving it would merely expose the next missing `-sys` crate (`gtk-sys` → `gtk+-3.0`, then `webkit2gtk-sys` → `webkit2gtk-4.x`).

## 4. Findings: pre-existing vs convoy-introduced

| Gate                                        | Pre-existing (on main@bed0c5a) | Introduced by this convoy | Classification                                                                                                              |
| ------------------------------------------- | ------------------------------ | ------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `cargo fmt --check`                         | PASS                           | —                         | Clean formatting (rustfmt.toml: edition 2021, max_width 100, tab_spaces 4).                                                 |
| `cargo clippy --all-targets -- -D warnings` | FAIL (dep compile: `gdk-3.0`)  | —                         | Pre-existing **environment** failure. Clippy never runs; the failure is in building `gdk-sys`, a third-party binding crate. |
| `cargo test`                                | FAIL (dep compile: `gdk-3.0`)  | —                         | Pre-existing **environment** failure. Identical blocker; no test binaries compiled or executed.                             |
| `npx tsc`                                   | PASS                           | —                         | Clean type-check (tsconfig: strict, noEmit, noUnusedLocals, noUnusedParameters).                                            |

**Summary.** Because the worktree branch is byte-identical to `main@bed0c5a` (`git diff main...HEAD` empty), the convoy introduces **no** code-level regressions. Of the four gates, two pass (`fmt`, `tsc`) and two are blocked by a pre-existing, environment-level dependency of the Tauri Linux webview stack that cannot be satisfied in this container (`gdk-3.0` / `webkit2gtk` system dev libraries; no root/sudo to install them). No production code, no timing tests, and no NEXORA-*.md / _local_.md files were modified or created.

## 5. Artifacts

- Baseline commit: `bed0c5a0d3547b40389162e069ad8a568297aa3c`
- Evidence log files (gitignored, retained on the worktree at `logs/`): `fmt.log`, `clippy.log`, `test.log`, `tsc.log`.
- This document: `GATES-FULL-EVIDENCE.md`.
