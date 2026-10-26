# Shell chrome — Dock gutter, shared reading axis, strip controls

One build, one evidence set: the three fixes meet on the same stage, so a single before/after
pair per size is honest. Real Tauri capture (`npm run tauri dev`), client area only, 125 %
display scaling — a 1200×800 CSS viewport is a 1500×1000 PNG.

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `before-1200x800.png` | 1200×800 | 1500×1000 | `7BDD4476C45A4788C170EC89120CA3B1` | 3704 | Before: the empty-state sentence starts at x≈48 and the rail cuts its first ~13 px ("o messages yet"); `Zen`, `+` and the tab `×` are black on the strip; the view switch says `Chat`. |
| `before-1024x720.png` | 1024×720 | 1280×900 | `FFB024D9ABCE415502A12BF9C26421B7` | 4087 | Before at the minimum window: same crop, same invisible controls. |
| `before-1536x800.png` | 1536×800 | 1920×1000 | `10253B1677CE083B8977AF201C50AA28` | 4129 | Before at the widest point: the crop is width-independent — the tell that a reserve is missing, not that spacing is wrong. |
| `after-1200x800.png` | 1200×800 | 1500×1000 | `ADE77F451E86BEE6DFA26B3BB09B558C` | 3967 | After: the sentence reads in full on the same left edge as the composer card (220), `Split`/`Zen`/`+`/`×` are quiet-grey controls, and the switches read `Conversation`/`Context` against `Chat`/`Agent`. |
| `after-1024x720.png` | 1024×720 | 1280×900 | `F72BCB093FF43A366F40A3189EF77A0F` | 4223 | After at 1024: same axis at 132, the longer `Conversation` label fits without crowding `Context`, nothing clips. |
| `after-1536x793.png` | 1536×793 | 1920×991 | `F752337510ACC8677AC802A66C23BAD4` | 4253 | After at 1536 wide: axis at 388, strip controls uniform, band still edge to edge. Height is 793, not 800 — the capture machine's work area caps a 1047 px window while the taskbar is shown; the width is exact and the fixes are horizontal. |

Every frame is distinct and carries >900 colours.

## Measured (CSS px, live DOM)

| viewport | empty-state / transcript left | composer left | Δ | column centre = window centre | rail | clipped |
|---|---|---|---|---|---|---|
| 1024×720 | 108 → **132** | 132 | 24 → **0** | 512 = 512 | 60 | yes → no |
| 1200×800 | 108 → **220** | 220 | 112 → **0** | 600 = 600 | 60 | yes → no |
| 1536×800 | 108 → **388** | 388 | 280 → **0** | 768 = 768 | 60 | yes → no |

## What each fix is

1. **Rail reserve at the container.** `--dock-width: 60px` is the single source of truth
   (`src/index.css`); the Dock sizes itself from it, and the stage reserves it
   symmetrically (`padding-inline` on `.nex-main-conversation`), so content never crosses
   x = 60 while the column centre stays at exactly half the window. Full-bleed bands keep
   their paint edge to edge; only content is inset.
2. **One reading measure.** The composer's 760 px centred column was already the measure for
   message rows; the empty state was the outlier, stretching to the stage box and
   left-aligning its copy. It now uses the same centred column, so the transcript, the empty
   state and the composer share one axis at every width (Δ = 0 above).
3. **Controls painted from a token.** A bare `<button>` carries the UA's
   `color: buttontext`; with no `color-scheme` declared that is black, and the `ghost`
   variant paints no rest colour, so five controls fell through to ≈1.05:1. One base-layer
   fallback `button { color: var(--color-on-surface-variant) }` fixes the class of bug while
   still losing to every variant's own `text-*` utility (layer order, not specificity).
4. **View switch renamed** to `Conversation`/`Context` (RU `Диалог`/`Контекст`), group label
   `Pane content` / `Содержимое панели`; the composer keeps `Chat`/`Agent` byte-identical.

## Not proven by these stills

Hover / focus-visible / pressed states, light and high-contrast themes (measured in the theme
block at 5.46:1 and 12.39:1), and the composer's type-into-field behaviour — the composer
layout itself is Phase C's draft PR, not this one.
