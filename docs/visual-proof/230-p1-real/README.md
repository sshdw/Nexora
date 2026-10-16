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
`navigator.language === "c"` and throws `RangeError: invalid language tag`
(V8/Chromium is lenient, which is why mock/Chromium shots never showed it).
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
| `01-cold-sidebar-loading-skeletons.png` | real Tauri + real backend | cold boot; sidebar, `Новый разговор`, `Поиск`, 7 loading skeletons, footer |
| `01b-cold-sidebar-loading-skeletons-repeat.png` | real Tauri + real backend | same beat on a second, independent Xvfb — proves the recipe is reproducible |
| `02-conversation-list-after-locale-fix.png` | real Tauri + real backend | resolved list with relative dates (`23h`, `Вчера`, `Oct 14/12/9`), groups `Активные`/`Архив` — the state that previously crashed |
| `03-hover-actions.png` | real Tauri + real backend | pointer over row 2: the 4-icon action group revealed (export, rename, archive, delete). A native title tooltip paints beside the cursor as a solid black box under software rendering (WebKit tooltip paint artifact, out of P1 scope — noted, not fixed) |
| `04-selected-row.png` | real Tauri + real backend | clicked row 1: tab strip, thread message, composer, selected row with actions, pointer parked on neutral canvas |
| `05-keyboard-actions.png` | real Tauri + real backend | fresh launch, ZERO clicks: 8× Tab lands focus on row 1's delete button (focus ring visible, `Удалить разг…` tooltip) — action group revealed by keyboard focus alone |
| `06-search-mid-typing-spinner.png` | real Tauri + real backend | `R` typed, caret visible, dropdown not yet open, circular spinner glyph at the input's trailing edge — caught inside the 250ms debounce (zoom-verified) |
| `07-search-results.png` | real Tauri + real backend | token-exact query `Refactor`: dropdown group `Разговоры` with the two matching conversations (a bare `R` matches nothing — FTS MATCH gets the query as supplied, `search.rs:58`) |
| `08-empty-with-query.png` | real Tauri + real backend | query `zzzqqqx`: `Ничего не найдено по запросу «zzzqqqx».` empty state in the dropdown |
| `09-narrow-850px.png` | real Tauri + real backend | window resized to 850×800 (`xwininfo`-verified): sidebar + compressed main pane reflow correctly |

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
The old environment-driven "paints one frame then dies" paragraph below is
kept as history — it describes the bug's symptom, now explained.

## What is NOT here, and why

The other eleven beats (inline rename, long RU title unclipped, hover actions,
keyboard-revealed actions, selected row, search mid-typing + inline spinner,
search results, empty-with-query, narrow <=900px, reduced-motion, loading
skeletons *after* the list resolves) are **absent because they are unreachable**,
not because they were skipped.

With `WEBKIT_DISABLE_COMPOSITING_MODE=1` — the only environment that makes the
Tauri window produce any frame at all — the WebView paints exactly one frame
roughly 1.0s–1.5s after launch and then the content layer stops producing frames
permanently. Measured, not assumed:

* window stays `Map State: IsViewable`, geometry pinned at `1200x800+0+0`;
* the app process and `WebKitWebProcess` stay alive, RSS flat at ~170 MB;
* no page reload (connection count to `:1420` is flat after the initial burst);
* no new lines in the app log;
* forced resize (x2), `windowunmap`+`windowmap`, `windowraise`,
  `windowactivate`, mouse jiggle, click and typing all fail to revive it;
* `import -window root`, `import -window <id>` and `xwd -id <id>` all agree:
  the window content is genuinely gone, it is not a capture-path artifact.

The frames above were caught inside that ~0.5s window. That is why the sidebar
still shows loading skeletons: the conversation list never survives long enough
to resolve on screen, so no interaction beat can be driven or photographed.

## Environment verdict

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
