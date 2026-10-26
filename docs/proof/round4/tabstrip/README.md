# Tab strip — controls and view labels

Two shell defects the real build showed, fixed together on one branch because they meet on
the same strip.

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `01-1200x800.png` | 1200×800 | 1500×1000 | `5BF83CB2E430B8BCBC766FDAD84F77E7` | 3967 | `Split`, `Zen` and `+` are readable quiet-grey controls at the strip's right end; the view switch reads `Conversation` / `Context` while the composer below still reads `Chat` / `Agent`; empty state clear of the rail. |
| `02-1024x720.png` | 1024×720 | 1280×900 | `63391782261AC5BC523FE531070E1CAB` | 4223 | Same states at the minimum window: the longer `Conversation` label fits the view switch without crowding `Context`, and the strip controls keep their tone. |
| `03-1536x800.png` | 1536×800 | 1920×1000 | `CE629A28C27713F7B37CA7ED351B60A8` | 4253 | Same at the widest check point; the strip's right cluster keeps its place and the label change does not reflow the row. |

Every frame is distinct and carries >900 colours.

## What was wrong

- `Split`, `Zen`, the new-tab `+`, the tab close `×` and the pane header's export control
  drew **black glyphs on the `#1a191f` strip** (≈1.05:1 — measured as invisible). Cause: a
  bare `<button>` carries the UA's `color: buttontext`, no `color-scheme` is declared, so
  `buttontext` resolves to black; the `ghost` variant those controls use paints no rest
  colour, so nothing overrode it.
- The stage's view switch and the composer's mode switch both said **"Chat"** in the same
  chip grammar, which made the conversation's mode ambiguous at a glance.

## What the fix is

- One base-layer fallback `button { color: var(--color-on-surface-variant) }` in
  `src/index.css`. It sits in `layer(base)`, which is declared before `utilities`, so every
  variant's own `text-*` / `hover:text-foreground` still wins by layer order — the filled
  button keeps `text-primary-foreground`. An unlayered rule would have repainted it.
- The view switch is renamed: EN `Conversation` / `Context`, RU `Диалог` / `Контекст`; its
  group label is now `Pane content` / `Содержимое панели` (the old "Conversation view" read
  as the option once the option itself is "Conversation"). The composer keeps `Chat` /
  `Agent` byte-identical.

## Not proven by these stills

Hover, focus-visible and pressed states (still captures show the resting state only), and
the light/high-contrast tone of the same controls — measured in the theme block at 5.46:1
and 12.39:1 but not captured here.
