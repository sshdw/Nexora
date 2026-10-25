# Phase C proof frames — composer + Glide Select

Real Tauri build (`npm run tauri dev`), client area only, 125% display scaling
(a 1200×800 CSS viewport is a 1500×1000 PNG). A conversation was opened through the
Dock's "New" button so the composer is mounted; the pointer was parked off every hover
target so no tooltip sits in a frame.

| File | CSS viewport | PNG | MD5 | Colours (full scan) | What I saw |
|---|---|---|---|---|---|
| `01-agent-1200x800-dark.png` | 1200×800 | 1500×1000 | `6100D6819ECBDE35056E66D327E9D105` | 3979 | Pane opens in Agent: the mode pill rests on "Agent", the autonomy pill rests on "Supervised" with an equal ring on both ends, the "Message" field is visible above the hint row, the model id `openai/gpt-oss-20b` is readable, Send is drawn in its disabled tone, and no attach control exists anywhere. |
| `02-agent-1024x720-dark.png` | 1024×720 | 1280×900 | `20C9841961F463DB86AC358553EB8B26` | 3979 | Same state at the minimum window: the composer keeps its four rows without wrapping, the field and the model id stay legible, nothing clips inside the card. |

Both frames are distinct and carry >900 colours.

## What these frames prove / do not prove

- Prove: Agent is the default mode in a real build; the attachment affordance is absent in
  Agent; the composer shows an editable field with its "Message" legend; the Glide Select
  pill is seated symmetrically inside its track in two groups (mode and autonomy) at two
  widths; Send is visibly unarmed on an empty draft.
- Do not prove: the pill's motion (a still cannot), the model picker's row layout with its
  popover closed (covered by tests), Chat-mode paint, running/approval/complete states, RU,
  light theme, 200 % zoom — those rows stay open for Phases D–F.

## Defect visible in these frames (not fixed here)

The conversation's empty-message block still runs under the Dock: the frame shows
"o messages yet" / "rite your first message below to start the conversation." because the
first ~60 px of the text sit behind the rail. The Dock is a fixed overlay and nothing in
the shell reserves its width, so any full-bleed content in that vertical band is clipped.
The gutter is not specified in the plan (`.ai/plans/nexora-shell-implementation-plan.md`
§5.2 names `--dock-width` but never assigns an inset), so this needs an owner decision
instead of a guessed 60 px padding.

## Fixed in this phase after the first QA pass

- The teaching legend painted an opaque card-toned plate over the textarea's first line, so
  the field looked absent. It is now a static row below the field.
- The model trigger had no `color` rule at all, so the UA's `buttontext` (black) painted a
  1.1:1 label; it now uses `--muted-foreground` (5.64:1 on the card).
- The pill was measured with integer `offsetLeft`/`offsetWidth` on fractional grid seats and
  lost its radius to a utility class, so it sat flush against the track's right edge; it is
  now published from `getBoundingClientRect` and shares the segment's radius token.
