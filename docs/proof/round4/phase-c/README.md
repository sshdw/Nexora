# Phase C proof frames — composer + Glide Select

Real Tauri build (`npm run tauri dev`), client area only, 125% display scaling
(a 1200×800 CSS viewport is a 1500×1000 PNG). A conversation was opened through the
Dock's "New" button so the composer is mounted.

| File | CSS viewport | PNG | MD5 | Colours (full scan) | What I saw |
|---|---|---|---|---|---|
| `01-agent-1200x800-dark.png` | 1200×800 | 1500×1000 | `43EA9242C6473257D0081DA76B954C3B` | 4079 | Pane opens in Agent: the mode pill rests on "Agent", the autonomy pill rests on "Full-auto", Send is present, and there is no attach control anywhere — the attachment affordance is absent rather than disabled. |
| `02-agent-1024x720-dark.png` | 1024×720 | 1280×900 | `E84A656C7DBE42D2B7CDBE51420B7A93` | 4140 | Same state at the minimum window: the composer keeps its rows, the pills keep their measured width, nothing wraps or clips inside the composer. |

Both frames are distinct and carry >900 colours.

## What these frames prove / do not prove

- Prove: Agent is the default mode in a real build; the attachment affordance is absent in
  Agent; the Glide Select pill rests exactly on the selected segment in two groups
  (mode and autonomy) at two widths.
- Do not prove: the pill's motion (a still cannot), the model picker's row layout with its
  popover closed (it is only reachable when the picker is open — covered by tests, not by
  these stills), Chat-mode paint (the mode switch was exercised in tests, not captured),
  running/approval/complete states, RU, light theme, 200 % zoom — those rows stay open for
  Phases D–F.

## Defect visible in these frames (not fixed here)

The conversation's empty-message block runs under the Dock: the frame shows
"o messages yet" / "rite your first message below to start the conversation." because the
first ~60 px of the text sit behind the rail. The Dock is a fixed overlay and nothing in
the shell reserves its width, so any full-bleed content in that vertical band is clipped.
The gutter is not specified in the plan (`.ai/plans/nexora-shell-implementation-plan.md`
§5.2 names `--dock-width` but never assigns an inset), so this needs an owner decision
instead of a guessed 60 px padding.
