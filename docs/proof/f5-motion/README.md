# F5 close-out proof frames — P7 motion contract, TextType/gsap deletion, D-01..D-14

Real Tauri dev build (`npm run tauri dev`), WebView2 driven over its CDP debugging port
(same method as `docs/proof/round4/round6/` and `r6-shell/`), client area only.
Branch `task/2.0-f5-design` at `6d17d77` (F5 code commit; Rust underneath = main
`31bb399` after #312). Real dev SQLite (schema v14); one real Chat-mode turn was sent
(`nvidia/nemotron-3-ultra-550b-a55b`) — it completed ("F5 Motion QA ping acknowledged —
systems nominal, ready for traffic."), so frame 07 is a settled assistant turn, not an
error card.

Viewport: 1200×800 CSS (900×800 for the narrow-window frame), rendered at DPR 1.0 via
CDP `Emulation.setDeviceMetricsOverride` — PNGs are exactly the CSS client area.

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `01-cold-start-1200x800.png` | 1200×800 | 1200×800 | `FD303F89A1867A11AA80951FCD9EABAD` | 4345 | Cold start: dock rail, composer card, empty thread. `data-motion-level="l3"` live on the root element (P7 adaptive band for a fresh session); zero TextType references remain. |
| `02-sessions-open-1200x800.png` | 1200×800 | 1200×800 | `2A00AD1C6C0114B57AFF399F8310AC27` | 4238 | Sessions panel open over the cold-start state (panel motion governed by the new token layer). |
| `03-tabs-two-1200x800.png` | 1200×800 | 1200×800 | `F596F8E4050CB135A8FAA85B16CF6ADB` | 2739 | Two "New Conversation" tabs in the strip, second active. |
| `04-tabs-split-1200x800.png` | 1200×800 | 1200×800 | `CB8FF726C957B89402E2750AE6077649` | 3711 | Split active: two panes, each with composer; Close split control present. |
| `05-conversation-loaded-1200x800.png` | 1200×800 | 1200×800 | `29E47AC8DC33F05A40FED89F5272E153` | 3495 | A real conversation loaded from the sessions panel — panel CLOSED after the pick (QA round 1 fixed the occlusion), single-pane state: full thread + composer visible. |
| `06-send-streaming-1200x800.png` | 1200×800 | 1200×800 | `8148CE83FF42E940E3A3251D8889336C` | 3560 | Real Chat-mode turn in flight (~1.1 s after send): assistant responding state visible in the thread. |
| `07-turn-final-1200x800.png` | 1200×800 | 1200×800 | `2136E0717933184789C2EBA2F450C114` | 3595 | Turn settled: assistant reply rendered in full, composer back to idle. |
| `08-sessions-scroll-1200x800.png` | 1200×800 | 1200×800 | `5D5A7536146DA66218D03829DFEE7042` | 4224 | Scroll journey state: sessions list mid-scroll (`scrollTop` 1800 of 4253 — older dated rows), thread visible behind. The dev thread itself is short and does not overflow, so the scrollable real content is the sessions list (honest substitution for "long conversation scroll"). |
| `09-narrow-900-1200x800.png` | 900×800 | 900×800 | `75E5E33E6DAD1C2BAAF84383BCC5E71B` | 3596 | Narrow-window journey state at 900 CSS width **after the QA-found overflow fix**: model label truncates with ellipsis inside its card, chevron + Send stay inside the card (round 1 showed the label running past the card edge and clipping at the window). |
| `10-hover-send-1200x800.png` | 1200×800 | 1200×800 | `B6EEF07DEC7AC7B6671B8F463134F0C8` | 3140 | Send button under real pointer hover with text in the composer (button ENABLED — round 1 hovered a disabled button and proved nothing): hover tone applied via the 120 ms token. |
| `11-press-send-1200x800.png` | 1200×800 | 1200×800 | `B0570D0DAE1F1516BE24E68FA2CDE3F0` | 3186 | Send captured mid-press while enabled (mousePressed held ~60 ms): P7 press interaction; the release completed a real send. Bytes differ from 10 — the press state is a distinct render. |
| `12-reduced-motion-panel-1200x800.png` | 1200×800 | 1200×800 | `78A7E1E87F4CB26F4B1991761C593A8B` | 5176 | **Live reduced-motion pass** (the r6 known gap, closed): `prefers-reduced-motion: reduce` emulated — `matchMedia` reports true, sessions panel opened under the global gate renders its final state with no mid-flight artifacts; adaptive level observed at `l1`. |

Every frame distinct (unique MD5), all ≥2739 colours. Adaptive level observed live:
`l3` at cold start → `l1` after the session's interactions (frames 01 vs 12) — the
§7.3 mechanism adapting in real use.

## QA round 1 → fix (recorded)

Independent visual QA FAILED the first capture set (7 frames): the sessions panel stayed
open from frame 05 onward (capture-script fault — occluded every "state" subject), and it
found a REAL narrow-width defect: at 900px the composer model row overflowed its card and
clipped at the window edge. The defect was fixed by an implementer round (root cause: the
vendored shadcn Button base string carries `shrink-0`, which — as a utility in
layer(utilities) — outranks the sheet's `min-width:0`; fix = `shrink` in the ModelPicker
trigger className so tailwind-merge evicts the bare `shrink-0`, leaving the SVG-scoped
scoping intact; `src/components/ModelPicker.tsx:144-156`, three new pins in
`Composer.test.tsx`, 981/981 green). Frames 05–11 were recaptured with the panel closed
and the mouse parked away from controls; frame 08 was reframed honestly to the sessions
list scroll (the dev thread is short and does not overflow). Frames 01–04, 12 passed
round 1 and are unchanged.

## §26 checklist (13, honest) — verified on these frames by the independent visual QA pass

Answers recorded in the QA verdict for this set; reject rule applied to the last two items.

## Known gaps

- Reduced-motion capture covers the panel-open journey state; per-interaction
  reduced-motion variants (press/hover under reduce) are code-pinned by the global gate
  (`motion.css:182-201` collapses durations AND delays) and the new token layer is
  reduced-motion-safe by construction.
- Frame 11 captures the press window (60 ms after mousePressed); the transform's
  end-state is inferable from the token spec rather than a second still.
