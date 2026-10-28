# 262 — REAL-Tauri capture evidence (tab strip + pane header at the 40px freeze)

**Every frame in this folder is REAL Tauri**: the `src-tauri/target/debug/nexora` WebKitGTK
binary running on this box, the real SQLite backend (`schema v14`), the real Vite dev server at
`http://localhost:1420`, the real thread `P3 — длинная ветка` open, RU chrome. No `?mock`, no
Chromium, no headless browser, no mock backend.

## Environment (measured on this box)

```
Xvfb :100 -screen 0 1600x1000x24 -ac +extension GLX +extension RENDER +render -noreset &
DISPLAY=:100 WEBKIT_DISABLE_COMPOSITING_MODE=1 /home/ubuntu/nexora/src-tauri/target/debug/nexora
```

Window: 1200x800 at (0,0) on `:100`, captured with `import -window <id>`.
`LIBGL_ALWAYS_SOFTWARE=1` / `GALLIUM_DRIVER=swrast` were deliberately NOT set (they break GL on
this box); the `libEGL DRI3/DRI2` warnings in the app's stderr are a cosmetic fallback notice.

Both frames are the SAME window, the SAME thread and the SAME scroll position. The only
difference is the CSS the dev server was serving: the "before" frame was taken with the tree at
`origin/main` (2cacffa), the "after" frame with this branch's `src/index.css` + `src/styles/tabs.css`
served by the same dev server (the two files were swapped into the serving worktree for the
capture and reverted immediately afterwards — `git status` in the serving worktree is clean).

## Frames

| File | Colours | md5 | What is in the frame | What I SAW |
|---|---|---|---|---|
| `01-before-header-52px.png` | 2234 | `7babad0644dd6f9e9f4d049227d7ffb9` | the tab strip and the pane header of the open thread, rendered from `origin/main` | the two chrome rows are 52px each; the pane header carries the title, the `p3-workspace` chip and the upload button |
| `02-after-header-40px.png` | 2249 | `e504fb2c90d2c49edac61f782739988d` | the same window, the same thread, with this branch's CSS served | both chrome rows are 40px; the pane header still carries title + chip + upload button, nothing clipped, nothing overlapping |

Both frames are distinct images (unique md5) and each name says what the frame shows.

## Measurement (pixel column scan, not eyeballed)

A single column of pixels was read down the window (`convert <frame> -crop 1x200+600+0 txt:-`),
and the colour runs give the row boundaries. Chrome is `rgb(26,25,31)` (`#1A191F`); the 1px
hairline under each chrome row is `rgb(39,38,44)`; the content surface is `rgb(30,29,36)`.

| Frame | tab strip | pane header | chrome total | content surface starts |
|---|---|---|---|---|
| `01-before` | rows 0..50 + hairline 51 → **52px** | rows 52..102 + hairline 103 → **52px** | **104px** | y=104 |
| `02-after` | rows 0..39 + hairline 40 → **40px** | rows 41..79 + hairline 80 → **40px** | **80px** | y=81 |

24px of chrome reclaimed, 12px per row — exactly the `NEXORA-THEME.md` §6 freeze
("Header 40px freeze stands (52 to 40 = 12px reclaimed)").

## How the 80px was reached (two rounds, both captured)

- **Round 1** moved the two rows from the shared legacy token `--nex-sys-layout-header-height`
  (52px) to a new flat `--chrome-height: 40px`. Captured on the real build it measured 40px for
  the tab strip but **48px for the pane header** (chrome 90px): `min-height` cannot shrink
  content, and the header's own row (title + chip + 32px button + 8px block padding) was 48px
  tall. Visual QA called that CRITICAL and blocked the merge.
- **Round 2** gives the pane header a definite `height: var(--chrome-height)` with 2px block
  padding, so the 32px control row fits inside a 40px border-box row. Captured on the real build
  it measures 40px + 40px = 80px of chrome, with the title, the chip and the button intact.

The five UN-migrated legacy consumers of `--nex-sys-layout-header-height` (activityHealth.css,
settings.css, prompts.css, versionControl.css, styles.css) are untouched and still render at
52px — they are outside this phase's surface and are asserted by `src/chromeHeight.test.ts`.

Refs #262
