# Phase C proof frames — composer + Glide Select

Real Tauri build (`npm run tauri dev`), client area only, 125 % display scaling (a 1200×800
CSS viewport is a 1500×1000 PNG). A conversation was opened through the Dock's "New" button
so the composer is mounted; the pointer was parked off every hover target so no tooltip sits
in a frame. Captured on top of the merged shell fixes (Dock gutter, shared reading axis,
strip controls), so the frames show the composer as it will actually ship.

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `01-agent-1200x800-dark.png` | 1200×800 | 1500×1000 | `4C923EFC90A0AD95D2948B255BC53045` | 4181 | Pane opens in Agent: the mode pill rests on "Agent", the autonomy pill rests on "Full-auto" with an equal ring on both ends, the "Message" field is visible above the hint row, the model id is readable metadata, Send is drawn unarmed, and no attach control exists anywhere. The empty state sits on the same left edge as the composer card. |
| `02-agent-1024x720-dark.png` | 1024×720 | 1280×900 | `CB62815F8C5BB6E3416AAE27B3B35B1E` | 4574 | Same state at the minimum window: the composer keeps its four rows without wrapping, the field and the model id stay legible, the axis holds at 164, nothing clips inside the card. |

Both frames are distinct and carry >900 colours.

## What these frames prove / do not prove

- Prove: Agent is the default mode in a real build; the attachment affordance is absent in
  Agent; the composer shows an editable field with its "Message" legend; the Glide Select
  pill is seated symmetrically inside its track in two groups (mode and autonomy) at two
  widths; Send is visibly unarmed on an empty draft; the reading column is shared with the
  transcript.
- Do not prove: the pill's motion (a still cannot), the model picker's row layout with its
  popover closed (covered by tests), Chat-mode paint, running/approval/complete states, RU,
  light theme, 200 % zoom — those rows stay open for Phases D–F.

## Fixed in this phase after the first QA pass

- The teaching legend painted an opaque card-toned plate over the textarea's first line, so
  the field looked absent. It is now a static row below the field.
- The model trigger had no `color` rule at all, so the UA's `buttontext` (black) painted a
  1.1:1 label; the app-wide base-layer fallback plus this phase's own rule make it metadata
  grey (5.64:1 on the card).
- The pill was measured with integer `offsetLeft`/`offsetWidth` on fractional grid seats and
  lost its radius to a utility class, so it sat flush against the track's right edge; it is
  now published from `getBoundingClientRect` and shares the segment's radius token.
