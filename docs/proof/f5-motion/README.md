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
| `05-conversation-loaded-1200x800.png` | 1200×800 | 1200×800 | `3469555D6C338B1397C4513EB06CC937` | 3613 | A real conversation loaded from the sessions panel; thread + composer ready. |
| `06-send-streaming-1200x800.png` | 1200×800 | 1200×800 | `F89650F71C755BECD2F01A433754EB6C` | 3682 | Real Chat-mode turn in flight (~1.2 s after send): assistant responding state visible. |
| `07-turn-final-1200x800.png` | 1200×800 | 1200×800 | `CABA0EAFAEE618AC5C6C539BC2BC9B93` | 5304 | Turn settled: assistant reply "F5 Motion QA ping acknowledged — systems nominal, ready for traffic." rendered; composer untouched. |
| `08-long-scroll-1200x800.png` | 1200×800 | 1200×800 | `BBF0D2FD1B05B21E2F8695174B3A541A` | 5127 | Thread scrolled to bottom over real content; scroll chrome per tokens. |
| `09-narrow-900-1200x800.png` | 900×800 | 900×800 | `BDDA9F44B5F852EA718D928B8274586F` | 4734 | Narrow-window journey state at 900 CSS width: layout reflows, no overflow, composer usable. |
| `10-hover-send-1200x800.png` | 1200×800 | 1200×800 | `4D442F83D5E275B27C6F7ACBC469C24D` | 5131 | Send button under real pointer hover (CDP mouseMoved): hover tone applied via the 120 ms token. |
| `11-press-send-1200x800.png` | 1200×800 | 1200×800 | `47EAC6AB0FC978D2DF9590386B3BD0BD` | 4728 | Send button captured mid-press (mousePressed held): P7 press interaction (transform/opacity, exit faster than enter). |
| `12-reduced-motion-panel-1200x800.png` | 1200×800 | 1200×800 | `78A7E1E87F4CB26F4B1991761C593A8B` | 5176 | **Live reduced-motion pass** (the r6 known gap, closed): `prefers-reduced-motion: reduce` emulated — `matchMedia` reports true, sessions panel opened under the global gate renders its final state with no mid-flight artifacts; adaptive level observed at `l1`. |

Every frame distinct (unique MD5), all ≥2739 colours. Adaptive level observed live:
`l3` at cold start → `l1` after the session's interactions (frames 01 vs 12) — the
§7.3 mechanism adapting in real use.

## §26 checklist (13, honest) — verified on these frames by the independent visual QA pass

Answers recorded in the QA verdict for this set; reject rule applied to the last two items.

## Known gaps

- Reduced-motion capture covers the panel-open journey state; per-interaction
  reduced-motion variants (press/hover under reduce) are code-pinned by the global gate
  (`motion.css:182-201` collapses durations AND delays) and the new token layer is
  reduced-motion-safe by construction.
- Frame 11 captures the press window (60 ms after mousePressed); the transform's
  end-state is inferable from the token spec rather than a second still.
