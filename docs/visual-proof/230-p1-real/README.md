# 230 Phase 1 — REAL-Tauri capture evidence

**Status: REAL-BUILD PROOF ACHIEVED for the P1 list/search beats below.**
The "one frame then permanently blank" failure mode is FIXED — it was a real
product bug, not a host-path fault (see "Root cause" below). Every frame here
is **REAL Tauri**: the `src-tauri/target/debug/nexora` WebKitGTK binary, real
SQLite backend (`schema v14`), real Vite dev server at `http://localhost:1420`.
No `?mock`, no Chromium. Each frame was opened and inspected before acceptance.

## Root cause (fixed in this branch)

`src/lib/format.ts` called `new Intl.DateTimeFormat(navigator.language, …)`
with no validation. On a C/POSIX-locale box WebKit's JavaScriptCore reports
`navigator.language === "c"` and throws `RangeError: invalid language tag`.
Both engines reject `"c"` (verified on node/V8: the constructor,
`supportedLocalesOf` and `getCanonicalLocales` all throw for it) — the
mock/Chromium shots never showed the bug only because their
`navigator.language` was a valid tag, never `"c"`.
With no error boundary above the list, the throw unmounted the whole tree:
skeletons painted (no timestamps yet), then rows arrived, `formatRelativeTime`
threw, the window went blank. Measured before the fix: exactly ONE painted
frame (~883–905 colours) then a permanent colour count of 1. After the fix:
947 colours sustained across 19 consecutive samples over ~10s.

Fix: `dateLocale()` resolves the tag through
`Intl.DateTimeFormat.supportedLocalesOf` with an `en-US` fallback, wrapped in
try/catch (both arms are load-bearing: V8's `supportedLocalesOf(["c"])`
*throws* `RangeError`; JSC returns `[]`). Regression test:
`src/lib/format.test.ts` (stubs `navigator.language = "c"`).

Recipe note: launch WITHOUT `LIBGL_ALWAYS_SOFTWARE=1` — `capture-tauri.sh`
documents that it breaks GL on this box. Used:
`WEBKIT_DISABLE_COMPOSITING_MODE=1` only, on Xvfb `:89`, openbox WM.

## Frames

| File | Evidence class | Content |
|---|---|---|
| `01-cold-sidebar-loading-skeletons.png` | real Tauri + real backend | cold boot; sidebar, `Новый разговор`, `Поиск`, 5 loading skeletons, footer |
| `01b-cold-sidebar-loading-skeletons-repeat.png` | real Tauri + real backend | same beat on a second, independent Xvfb — proves the recipe is reproducible |
| `02-conversation-list-after-locale-fix.png` | real Tauri + real backend | resolved list with relative dates (`23h`, `Вчера`, `Oct 14/12/9`), groups `Активные`/`Архив` — the state that previously crashed |
| `03-hover-actions.png` | real Tauri + real backend | pointer over row 2: the 4-icon action group revealed (export, rename, archive, delete). A native title tooltip paints beside the cursor as a solid black box under software rendering (WebKit tooltip paint artifact, out of P1 scope — noted, not fixed) |
| `04-selected-row.png` | real Tauri + real backend | clicked row 1: tab strip, thread message, composer, selected row with actions, pointer parked on neutral canvas |
| `05-keyboard-actions.png` | real Tauri + real backend | fresh launch, ZERO clicks: 8× Tab lands focus on row 1's delete button (focus ring visible, `Удалить разг…` tooltip) — action group revealed by keyboard focus alone |
| `06-search-mid-typing-spinner.png` | real Tauri + real backend | `R` typed, caret visible, dropdown not yet open, circular spinner glyph at the input's trailing edge — caught inside the 250ms debounce (zoom-verified) |
| `07-search-results.png` | real Tauri + real backend | token-exact query `Refactor`: dropdown group `Разговоры` with the two matching conversations (a bare `R` matches nothing — FTS MATCH gets the query as supplied, `search.rs:58`) |
| `08-empty-with-query.png` | real Tauri + real backend | query `zzzqqqx`: `Ничего не найдено по запросу «zzzqqqx».` empty state in the dropdown |
| `09-narrow-850px.png` | real Tauri + real backend | window resized to 850×800 (`xwininfo`-verified): sidebar + compressed main pane reflow correctly |
| `10-list-error.png` | real Tauri + real backend | the migrated list error state (`ConversationList.tsx:72-90`): rename rejected by the backend 500-char CHECK constraint renders the bordered `role="alert"` panel (`text-destructive` message + `Повторить` retry). Triggered by typing a 501-char title into the inline rename field; backend logs the failure, the DB is unchanged (integrity ok, 13 rows), relaunch restores the list |

Colour counts: 1221 and 1218 unique colours in the 1200x800 app body for the
skeleton frames (a blank window measures 1–6); 947 sustained for the resolved
list (darker surface than the skeleton flash frame).

## What is NOT here, and why

Reduced-motion is **not real-build proven**: `gtk-enable-animations=0` (via a
scratch `XDG_CONFIG_HOME`) was attempted and the app renders identically, but
whether WebKitGTK maps that setting to `prefers-reduced-motion: reduce` cannot
be verified from outside the WebView, and a still cannot distinguish the two
states anyway — so no frame here claims that beat. It stays a Chromium
supplement. Likewise inline-rename and the long-RU-title close-up were already
covered by Chromium supplements and were not re-driven here; nothing about
them changed in this fix.
The search error panel (`SearchBox.tsx:145-153`) is not shot either: every
typed query goes through the backend `sanitize_fts_query`, which quotes each
token, so user input can never form an invalid FTS5 MATCH expression (a lone
`"`, `"unclosed`, `*`, `a OR` all yield empty results, never a backend
error) — only an index-level failure renders it, which needs index sabotage
on a scratch DB. The busy/disabled row states are transient by design
(milliseconds-long round-trips) and not photographable. The error panel's own
retry click did not fire on this box (geometry-verified; sibling buttons in
the same session respond; no backend call logged) — relaunch restores the
list, but the click itself needs owner-box verification. Follow-ups: #249
(search error, busy states, retry), #250 (rename/long-RU/reduced-motion
re-shoot), #251 (native-tooltip paint artifact), #252 (en-US date fallback).

## Environment verdict (superseded 2026-10-16 — kept as history)

The paragraph below was written BEFORE the locale fix, when the evidence
pointed at the Tauri host path. It is kept verbatim as history: the minimal
`WebKitWebView` held paint because it never resolved IPC rows, so the
crashing code path never ran there. The fault was app code
(`src/lib/format.ts`), fixed in this branch — see "Root cause" above.

The machine is exonerated. A minimal WebKitGTK 4.1 `WebKitWebView` loading the
**same URL** on the **same Xvfb** renders and holds indefinitely (1175 colours,
stable across many samples). So the fault is in the Tauri host path, not the
GPU stack, the dev server, or the backend.

See `../capture-tauri.sh` (header carries the full measured environment and the
attempted-and-rejected variables) and PR #247 / issue #245.

## Reproducing

```sh
# succeeds only once the Tauri WebView sustains paint; currently exits 3 by design
docs/visual-proof/capture-tauri.sh /tmp/frame.png --display :77

# the flash-window capture actually used for the two frames above
Xvfb :78 -screen 0 1600x1000x24 -ac +extension GLX +extension RENDER +render -noreset &
DISPLAY=:78 WEBKIT_DISABLE_COMPOSITING_MODE=1 src-tauri/target/debug/nexora &
# then sample with xwd -root and keep the frame with the most unique colours
```
