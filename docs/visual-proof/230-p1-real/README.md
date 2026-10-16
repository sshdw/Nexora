# 230 Phase 1 — REAL-Tauri capture evidence

**Status: PARTIAL. Real-build proof is NOT achieved.** This directory holds the
genuinely reachable real-Tauri frames plus the reason the rest are missing.

Every frame here is **REAL Tauri**: the `src-tauri/target/debug/nexora` WebKitGTK
binary, real SQLite backend (`schema v14`, 12 active conversations seeded),
real Vite dev server at `http://localhost:1420`. No `?mock`, no Chromium.

## Frames

| File | Evidence class | Content |
|---|---|---|
| `01-cold-sidebar-loading-skeletons.png` | real Tauri + real backend | cold boot; sidebar, `Новый разговор`, `Поиск`, 7 loading skeletons, footer |
| `01b-cold-sidebar-loading-skeletons-repeat.png` | real Tauri + real backend | same beat on a second, independent Xvfb — proves the recipe is reproducible |

Colour counts: 1221 and 1218 unique colours in the 1200x800 app body (a blank
window measures 1–6).

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
