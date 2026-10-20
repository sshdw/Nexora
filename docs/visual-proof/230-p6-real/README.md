# 230 Phase 6 — kill legacy tokens.css: REAL-Tauri pixel proof

Every frame in this folder is REAL Tauri: the `src-tauri/target/debug/nexora`
WebKitGTK binary, the real SQLite backend, the real Vite dev server, the real
OS keyring. No `?mock`, no Chromium, no headless browser, no mocked IPC.

Branch: `task/oci-230-p6` @ `c341a85` (P6 kill legacy, mechanical token swap
only — VALUES UNCHANGED, names/locations relocated from `tokens.css` into
`src/index.css`).

## Proof method

Proof 2 (parity): per-(theme, token) resolved-literal parity test in
`src/theme.test.ts` with committed rename map `src/test/parityMap.ts` — dark,
light and high-contrast all resolve to their previous literals.

Proof 3 (pixels): BEFORE (base commit `80407b2`, `tokens.css` present) vs AFTER
(P6 commit `c341a85`, `tokens.css` deleted) captured from the REAL Tauri build
at 1200x800, compared with `compare -metric AE` — MUST be 0.

## Frames

| File | Colours | What is in the frame | AE vs counterpart |
|---|---|---|---|
| `01-dark-main-before.png` | 1065 | BEFORE: dark theme, main screen, empty state, populated sidebar | AE=0 vs after (byte-identical md5) |
| `01-dark-main-after.png` | 1065 | AFTER: dark theme, main screen, empty state, populated sidebar | AE=0 vs before (byte-identical md5) |
| `02-dark-palette-before.png` | 1327 | BEFORE: dark theme, command palette open (Ctrl+K) | AE=0 vs after (byte-identical md5) |
| `02-dark-palette-after.png` | 1327 | AFTER: dark theme, command palette open (Ctrl+K) | AE=0 vs before (byte-identical md5) |

BEFORE and AFTER frames are byte-identical (same md5) — the token migration
changed names/locations only, zero values changed, zero visual difference.

AE=0 on the main screen (the most complex surface) confirms the token
migration changed nothing visually. AE=0 on the palette confirms the same for
the overlay surface.

High-contrast capture NOT possible: `useAppearance` accepts `dark|light` only,
no HC toggle UI, `devtools` not declared in Tauri config. HC verified by
parity test (Proof 2) instead.

Hidden panels (PromptLibrary, Activity, Audit, Debt, Issues, Docs, Release)
have no rail entry and no palette command — not accessible from the UI.
Verified by parity test instead.

## Verification

- `compare -metric AE before after /dev/null` → 0 (main screen), 0 (palette)
- `npx tsc` → exit 0
- `npx vitest run` → 625 passed / 25 files / 0 failed
- `python3 scripts/check-contrast.py` → ALL PASS, exit 0
- CI: fmt/clippy/cargo test/frontend all green

Effort: high
