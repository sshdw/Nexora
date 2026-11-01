# Round 6 proof frames — workspace in the composer, Zen gone, session actions back

Real Tauri dev build (`npm run tauri dev`, WebView2 with a CDP debugging port so the
frames could be driven without synthetic OS keystrokes), client area only, 125 % display
scaling (a 1200×800 CSS viewport is a 1500×1000 PNG). One conversation open, pointer
parked off every hover target.

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `01-empty-1200x800.png` | 1200×800 | 1500×1000 | `8F77D1DA5E82A5A02648B9CFE1375AAA` | 1912 | The Dock is four destinations — New, Sessions, Activity, Settings — with **no Workspace item**. The pane header is title + export only (no workspace chip, no Zen). The composer is the screen: one 760 card, centred, with `📁 nx · ⑂ feat/shell-round6 ⌄` above it; the chevron sits **next to the branch**, not at the card's right edge. Bottom row: `Chat \| Agent`, `Full-auto ⌄`, a solid empty context ring, `openai/gpt-oss-20b ⌄`, and Send as a **muted filled plate** with a light arrow. |
| `02-empty-1024x720.png` | 1024×720 | 1280×900 | `77A9B684B2A1BD11DD0D33B11E31A323` | 1908 | Same at the minimum window: the card keeps 760, the workspace row still aligns with the field's text, ring + model + Send stay on one row, nothing clips or wraps. |
| `03-empty-1536x800.png` | 1536×800 | 1920×1000 | `6BE72D5EC05A36F54574909C7EC4DDAA` | 1908 | Same at the widest check point: the card stays 760 and centred, the extra width is air on both sides. |
| `04-armed-send-1200x800.png` | 1200×800 | 1500×1000 | `01196F108CF8A3AD047151EDC7DFA535` | 2163 | The same button with a draft: the plate turns **light** and the arrow dark — the one high-contrast element on the screen. Nothing else moved. |
| `05-workspace-menu-1200x800.png` | 1200×800 | 1500×1000 | `E053BCF6646E5A4EEBE96438EF135ADE` | 2348 | The workspace control open: current root with a check, **Recent** (MRU, real paths), **Workspaces** (registry, with an unregister ×), and one **Choose folder…** row. No `Repos`, no `Start from scratch` — there is no data behind either. |
| `06-model-picker-1200x800.png` | 1200×800 | 1500×1000 | `7C78526B7B1891B7D8CC8D38F8BE8840` | 2598 | Model picker rebuilt: search field on top, vertical rows, long ids wrap (`nemotron-3-nano-omni-30b-a3b-reasoning` on two lines), selected row washed with a tick, **no horizontal scrollbar**, and the popover ends **9 px above the composer card** instead of overlapping it. |
| `07-context-popover-1200x800.png` | 1200×800 | 1500×1000 | `6D4F32100BB2A90E23FFD452ADFD606C` | 2483 | The ring's popover: `n/a` at the top and `Usage n/a` in the rows (never `0%` when `has_token_data=false`), no fake breakdown, compact one-column facts, budget below the fold. It is fully on screen (top 8 px) and ends 8 px above the card. |
| `08-sessions-1200x800.png` | 1200×800 | 1500×1000 | `7C3A28E5DA8E22C8BDD998801467DBE7` | 2249 | Sessions panel: **no horizontal scrollbar**, a thin styled vertical bar with a visible thumb and no platform arrows, rows readable at 300 px. |
| `09-sessions-actions-1200x800.png` | 1200×800 | 1500×1000 | `5CB6A02549E4A6BBA381483B8805360A` | 2396 | The first row's actions revealed by **keyboard focus** (not hover): pencil, archive, trash, with the `Rename conversation` tooltip portalled outside the scroller so it cannot widen it. |

Every frame is distinct and carries >1900 colours.

## What these frames prove / do not prove

- Prove: the empty screen is the composer with the workspace control above it and the
  chevron attached to its label (R6-03/R6-09); the Dock has no Workspace destination
  (R6-07); Zen and the Conversation/Context switch are absent from the rendered chrome
  (R6-04/R6-05); Send has a filled plate in idle and armed states (R6-01); the model
  picker searches, wraps and does not overflow sideways (R6-02); the context popover is
  honest about `n/a`, stays on screen and off the card (R6-05); Sessions has no
  horizontal bar and its rename/archive/delete actions exist (R6-06); scrollbars are
  thin, dark and arrow-free (R6-08).
- Do not prove: the stop state and the interrupt itself (no provider run was started —
  the frame set was shot without a live model call); hover-revealed actions (frame 09
  uses focus, which is the same affordance for keyboard users but not the pointer path);
  the workspace picker's native folder dialog; delete confirmation; inline rename;
  light theme; 200 % zoom; RU; motion and reduced motion.

## How the frames were driven

The app was started with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222`
and the state was set through CDP (`Input.dispatchMouseEvent`, a native-setter `input`
event for the draft). No synthetic OS keystrokes were used: on this machine `SendKeys`
does not reach WebView2 at all, which is why earlier rounds' typing artefacts are absent
here — the draft text is exact.
