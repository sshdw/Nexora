# Dock gutter — before / after

Owner decision B, with the constraints that decided the shape of the fix: reserve the rail
at the **layout container**, keep one axis for the thread and the composer, clear the 60 px
Dock in the empty state, and do not shift a centred column by half the rail.

Real Tauri capture, client area only, 125 % display scaling (a 1200×800 CSS viewport is a
1500×1000 PNG). One conversation is open in every frame so the stage, its empty state and
the composer are all mounted.

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `01-1200x800.png` | 1200×800 | 1500×1000 | `7BDD4476C45A4788C170EC89120CA3B1` | 3704 | Before: the empty state starts at x≈48 CSS and the rail cuts its first ~13 px — "o messages yet", "rite your first message…". |
| `02-1024x720.png` | 1024×720 | 1280×900 | `FFB024D9ABCE415502A12BF9C26421B7` | 4087 | Before at the minimum window: same crop, one glyph worse because the rail sits closer to the text column. |
| `03-1536x800.png` | 1536×800 | 1920×1000 | `10253B1677CE083B8977AF201C50AA28` | 4129 | Before at the widest check point: the crop is still there — it is independent of window width, which is the tell that the cause is a missing reserve, not a spacing choice. |
| `after-01-1200x800.png` | 1200×800 | 1500×1000 | `FB8FADC6802EF6FC9DF027349133B22E` | 3745 | After: the sentence reads in full and starts at 108 CSS, 48 px clear of the rail. The composer card has not moved (left 220, width 760, centre 600). The footer band still spans the window edge to edge, so no vertical seam is introduced. |
| `after-02-1024x720.png` | 1024×720 | 1280×900 | `FA41AE37497B7925FCD66C472B3AD067` | 4128 | After at 1024: full sentence, no crowding, band still full-bleed, composer centre 512 = window centre. |
| `after-03-1536x800.png` | 1536×800 | 1920×1000 | `BABB504602EFD056ADCF2A8F0DE30D1B` | 4170 | After at 1536: full sentence, composer centre 768 = window centre, no seam at either band edge. |

Every frame is distinct and carries >900 colours.

## Measured (CSS px, live DOM)

| viewport | thread content left before → after | composer left / width | composer centre vs window centre | clipped by rail |
|---|---|---|---|---|
| 1200×800 | 48 → 108 | 220 / 760 (unchanged) | 600 = 600 | yes → no |
| 1024×720 | 48 → 108 | 132 / 760 (unchanged) | 512 = 512 | yes → no |
| 1536×800 | 48 → 108 | 388 / 760 (unchanged) | 768 = 768 | yes → no |

## How it is done

- `--dock-width: 60px` is the single source of truth (`src/index.css`); the Dock sizes itself
  from it, so no second copy of the rail width exists.
- The stage container (`.nex-main-conversation`) reserves it **symmetrically**
  (`padding-inline`), so the content box never crosses x = 60 while its centre stays at
  exactly half the window — that is why the composer does not drift to the right.
- Surfaces that were full-bleed keep their paint edge to edge (the composer band, the stage
  chrome row); only content is inset. Nothing was fixed with a margin on the text.
