# 230 Phase 2 — REAL-Tauri capture evidence (tab strip + pane headers)

**Every frame in this folder is REAL Tauri**: the `src-tauri/target/debug/nexora` WebKitGTK
binary, freshly built from this branch (`23bb611`), real SQLite backend (`schema v14`), real
Vite dev server at `http://localhost:1420`. No `?mock`, no Chromium, no headless browser.
There are no Chromium supplements in this folder — every frame was captured from the real
window and every one was opened and inspected before acceptance.

## Environment (measured on this box)

```
Xvfb :97 -screen 0 1600x1000x24 -ac +extension GLX +extension RENDER +render -noreset &
DISPLAY=:97 openbox &
DISPLAY=:97 WEBKIT_DISABLE_COMPOSITING_MODE=1 /home/ubuntu/nexora/src-tauri/target/debug/nexora
```

`WEBKIT_DISABLE_COMPOSITING_MODE=1` is the only env var needed. `LIBGL_ALWAYS_SOFTWARE=1` and
`GALLIUM_DRIVER=swrast` were deliberately NOT set — they break GL on this box. The `libEGL
DRI3/DRI2` warnings in the app's stderr are a cosmetic fallback notice, not the fault.
Window: 1200x800 at (200,107), captured with `import -window`.

## Frames

| File | Colours | What is in the frame | What I SAW |
|---|---|---|---|
| `01-cold-start-empty.png` | 1178 | cold boot, zero tabs | No tab-strip row at all — the strip contributes zero height when `tabs.length === 0`, so the EmptyState owns the full column. No orphan 40px/52px row. |
| `02-two-tabs.png` | 2065 | one tab, `Сплит` disabled | Disabled below two open tabs. Pane header: title + workspace chip + export. |
| `03-two-tabs-plus.png` | 2455 | 4 tabs after two `+` presses | `Сплит` now **enabled**. Quiet text tabs, no pill, no wash. |
| `04-split-open.png` | 2443 | **split OPEN, two panes** | **EXACTLY ONE Split control in the whole app** — "Закрыть сплит", in the TAB STRIP. Neither pane header carries one. Each header: title + `agent_workspace` chip + export, nothing else. The `2` split badge is on the secondary tab. |
| `07a-chat-mode.png` | 2390 | Chat pane | Baseline for the toggle comparison. |
| `07b-context-mode.png` | 2363 | **Context pane after toggle** | `Контекст` pill active; content swapped in place. Tab strip and pane header did NOT move — no content shift. |
| `08-narrow-850.png` | 2299 | window resized to **850x800** (`xwininfo`-verified) | **This is the frame that proves overflow**: the third tab is cut mid-title at the strip's right edge where `Сплит` begins, while the actions stay pinned. Header (title + chip + export) still fits at 850px. Nothing clipped off-screen. |
| `09-workspace-chip-tooltip.png` | 2963 | pointer over the workspace chip | **Chip reads `agent_workspace` — the folder NAME.** The native tooltip shows the FULL path `/home/ubuntu/.local/share/com.nexora.app/agent_workspace`. This is MAP row 5 exactly. Captured from the X root (not `-window`) because the tooltip is a separate override-redirect window. |
| `10-tab-focus.png` | 2630 | keyboard focus (XTEST) | The focus ring is clearly rendered on the sidebar `Поиск` input. Proves the focus treatment paints in the real build. |
| `10x-crop-tabstrip.png` | 539 | supplement: close crop of the strip | 5 tabs, each with `×`. Active tab is visibly **heavier and brighter** with the tick beneath it; inactive tabs are dimmed. **No pill, no wash, no accent-coloured label** — MAP row 4's grammar holds. Actions pinned right. |

### Frames that were deliberately dropped

`05-overflow` and `06-overflow-scrolled` were captured and then **deleted** rather than
submitted. At 1200px five tabs fit without overflowing, so the filenames claimed more than the
frames showed. `08-narrow-850` proves the same behaviour for real.

Related, recorded so it is not misread as a defect: an earlier pass appeared to cap at five
tabs. There is **no tab cap in the code** — `openIds` grows unbounded
(`src/lib/useConversationTabs.ts:113`). The five-tab reading was an artifact of clicking a
hardcoded coordinate while the strip re-laid out between clicks, not product behaviour.

## Measured, not eyeballed

- **Split controls in the DOM, by eye on `04-split-open.png`: exactly 1.** With two panes
  open there is still only the single tab-strip control. Before this phase the header rendered
  a second one per pane, so a split offered four identical buttons.
- **Active tab tick colour: `#C9A66B` = `srgb(201,166,107)`** — sampled from the PNG, not
  eyeballed:
  `convert 02-two-tabs.png -crop 170x6+270+37 +repage -colors 6 -format %c histogram:info:`
- **Workspace chip text is achromatic** (`#918D88`, `#84807C` sampled) — correct per THEME §1.
- **Tab strip + pane header heights: 52px each.** Measured by scanning a pixel column down
  `10-tab-focus.png`: rows y=0..103 are uniformly `srgb(26,25,31)` and the content surface
  begins at **y=104** (= 52 + 52). See Findings #1 — this is a THEME §6 violation.

## Findings

### 1. CRITICAL — chrome rows render at 52px, THEME §6 freezes them at 40px

`NEXORA-THEME.md` §6 states verbatim: *"Header 40px freeze stands (52 to 40 = 12px
reclaimed)."* Both rows Phase 2 owns measure **52px**, because both read
`min-height: var(--nex-sys-layout-header-height)` and `src/styles/tokens.css:425` still sets
that token to `52px`. Across the phase's two surfaces that is 24px more chrome than the
contract allows.

**Not introduced by `53bfc6d`** — the same token backs the same two rules at `10b9631`. But
Phase 2 is the migration PR for exactly these two surfaces, and MAP row 5 / SYSTEM §10 make
this phase the owner of them.

**Not fixed here, and that is a real gap.** `--nex-sys-layout-header-height` is also consumed
by five un-migrated legacy sheets (`activityHealth.css:25`, `settings.css:24`, `prompts.css:24`,
`versionControl.css:25`, `styles.css:92`). Retargeting the shared token to 40px would silently
re-skin five out-of-scope surfaces, which #230 forbids. The correct fix is a scoped
chrome-height token for `tabs.css` only — which requires a code change this role does not
make. Declared in the PR Known-Gaps table and tracked as **#262**.

### 2. Non-achromatic accent on the chrome — already tracked, NOT a new finding

The active-tab tick samples `#C9A66B` (warm gold), where THEME §1 requires the chrome to be
achromatic (`c < 0.02`). This is **already declared** as item #2 of issue **#236**
("legacy M3E amber carriers survive the mono repaint (P6 residue)"), which records the exact
chain: `--nex-sys-color-accent-indicator` → `--nex-sys-color-primary` →
`--nex-ref-palette-amber-60` at `src/styles/tokens.css:192,213`. That token chain is
**byte-identical at `10b9631`** — P2 neither introduced nor re-blessed it. It dies with
`tokens.css` in Phase 6, which is the phase that owns the legacy sheet.

### 3. SUGGESTION — coloured legend dots in the Context panel

`07b` / `08` show amber/green/yellow dots beside `Пользователь 0.0%`, `Ассистент 0.0%`,
`Вызовы инструментов 0.0%`, `Прочее 0.0%`. THEME §1 reserves green/red/yellow for
success/error/warning **state signals**; these are category legend swatches on a progress
breakdown, i.e. decorative. MAP row 20 (Context panel) is not a Phase 2 surface and the panel
is untouched by this phase. Noted for the row-20 migration; not fixed here.

## Not captured, and why

- **Focus ring on a tab specifically.** `xdotool key --window` uses XSendEvent, which WebKit
  ignores. Driving with XTEST works and the ring is visible on the sidebar search input
  (`10-tab-focus.png`), but 15 Tab presses never landed on a tab label, so no frame shows a
  focused tab. The tablist keyboard contract itself (arrows / Home / End, `aria-selected`
  following) is pinned by `src/components/ConversationTabs.test.tsx` — this is a capture
  limitation, not an untested behaviour.
- **Reduced-motion frame.** `gtk-enable-animations=0` was already shown in the P1 proof to be
  unverifiable from outside the WebView, and a still frame cannot distinguish the two states.
  The press token's reduced-motion arm is verified in the built CSS instead — see the PR body.
- **A long/ellipsized workspace name.** The installed workspace root is
  `agent_workspace` (17 chars), which does not exceed the chip's cap, so no ellipsis case
  could be produced on this box. `WorkspaceChip`'s truncation is unit-tested instead.
- **Tab hover vs selected as separate kinds.** Visible in `10x-crop-tabstrip.png` for the
  selected state; a pointer-over-inactive-tab frame was not separated out.

## Reproducing

```sh
# 1. dev server (real backend via the Tauri IPC layer)
npx vite --port 1420 --strictPort

# 2. real window
Xvfb :97 -screen 0 1600x1000x24 -ac +extension GLX +extension RENDER +render -noreset &
DISPLAY=:97 openbox &
DISPLAY=:97 WEBKIT_DISABLE_COMPOSITING_MODE=1 \
  /path/to/target/debug/nexora &

# 3. capture (window only, no WM decoration)
W=$(for w in $(DISPLAY=:97 xdotool search --name 'Nexora'); do
      DISPLAY=:97 xwininfo -id $w | grep -q 'IsViewable' && echo $w; done | tail -1)
DISPLAY=:97 import -window "$W" frame.png

# 4. validity check — a real app body measures 1000-2500 unique colours.
#    A content-layer stall measures ~200-450. Never submit a low-colour frame.
convert frame.png -format %k info:
```

`docs/visual-proof/capture-tauri.sh` (merged in #247) automates the single-shot variant. It
tears down the app per shot, so a multi-beat session drives one instance as above.

## Capture hazards hit on this run (so the next run does not lose an hour to them)

- **Paint stall.** A resize can wedge the content layer; frames then measure ~200 colours.
  Re-capture after a pointer move, or re-launch.
- **Tooltip hijacks the window picker.** `xdotool search --name Nexora` also matches the
  native tooltip's override-redirect window, so a parked pointer makes `import -window`
  capture a 1040x75 tooltip instead of the app. Move the pointer away and wait for the
  tooltip to dismiss before capturing, and pick the window by size/`IsViewable`, not by first
  name match.
- **Split needs two tabs first.** `Сплит` is disabled below two open tabs; clicking it with
  one tab does nothing and produces a frame that looks like a failed split.
