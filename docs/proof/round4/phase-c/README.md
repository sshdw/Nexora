# Phase C proof frames — composer + Glide Select

Real Tauri build (`npm run tauri dev`), client area only, 125 % display scaling (a 1200×800
CSS viewport is a 1500×1000 PNG). A conversation was opened through the Dock's "New" button
and focus was moved off the Dock before each shot, so no tooltip sits in a frame. Captured on
top of the merged shell fixes (Dock gutter, shared reading axis, scroll viewports, strip
controls), so the frames show the composer as it will actually ship.

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `01-agent-1200x800-dark.png` | 1200×800 | 1500×1000 | `8EAADD2CF635EB92C33B68A7FD3FEDD1` | 4138 | Pane opens in Agent: the mode pill rests on "Agent", the autonomy pill rests on "Full-auto" with an equal ring on both ends, the field's placeholder reads "Ask anything, or describe a task" on the same left edge as the teaching row and the pill row beneath it, the model id shows in full (`nvidia/nemotron-3-super-120b-a12b`), Send is drawn unarmed with no border, and no attach control exists anywhere. The empty state, the placeholder and the composer card share one left edge. |
| `02-agent-1024x720-dark.png` | 1024×720 | 1280×900 | `B62DDE81B4189609E9F579A5B4FAEBAB` | 4145 | Same state at the minimum window: the composer keeps its four rows without wrapping, the placeholder, teaching row and pills stay on one edge, the model id still shows in full, nothing clips inside the card, and the axis holds at 132 CSS. |
| `03-agent-1536x800-dark.png` | 1536×800 | 1920×1000 | `DCF216F09A503ACE77499FE912EB0F1D` | 4138 | Same at the widest check point: the 760 px card stays centred at 388 CSS, the strip's right cluster keeps its place, and the wider window adds air on both sides of the column rather than stretching the measure. |

Every frame is distinct and carries >900 colours.

## What these frames prove / do not prove

- Prove: Agent is the default mode in a real build; the attachment affordance is absent in
  Agent; the field carries an instructive placeholder (not a noun) with no chrome of its own;
  all four rows inside the card share one inner inset; the model id is readable in full at all
  three widths; Send is visibly unarmed on an empty draft; the Glide Select pill is seated
  symmetrically inside its track in two groups at three widths; the reading column is shared
  with the transcript and the empty state.
- Do not prove: the pill's motion (a still cannot), the model picker's row layout with its
  popover closed (covered by tests), Chat-mode paint, running/approval/complete states, RU,
  light theme, 200 % zoom, hover/focus-visible — those rows stay open for Phases D–F.

## Fixed in this phase after the visual passes

- The teaching legend painted an opaque card-toned plate over the textarea's first line, so
  the field looked absent. It is now a static row below the field.
- The model trigger had no `color` rule at all, so the UA's `buttontext` (black) painted a
  1.1:1 label; the app-wide base-layer fallback plus this phase's own rule make it metadata
  grey (5.64:1 on the card).
- The pill was measured with integer `offsetLeft`/`offsetWidth` on fractional grid seats and
  lost its radius to a utility class, so it sat flush against the track's right edge; it is
  now published from `getBoundingClientRect` and shares the segment's radius token — one
  `--seg-radius` declaration governs both the seat and the pill.
- The placeholder was the noun "Message", which read as a form label with nothing under it.
  It now instructs, while the accessible name stays the short noun.
- Rows inside the card started on four different left edges (autonomy 3.2, placeholder 16,
  teaching 16.8, bar track 8.8 CSS px) because the field's shared-class inline padding
  double-counted the shell's inset. One inset now lives on the container; the rows declare
  none, and the field resets the shared padding explicitly.
- Send carried a full hairline and a mid-grey label, so it read as an enabled secondary
  button on an empty draft. Unarmed is now borderless and unfilled; armed is the light fill.
