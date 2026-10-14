/* ============================================================
   Autonomy explainer — the composer's one animated line (#220)

   WHAT IT IS. One dim sentence under the autonomy pills that
   says what the selected mode actually does — the mode names
   ("Supervised", "Semi-auto", "Full-auto") are labels nobody
   can act on without that sentence. It types itself in with
   React Bits' TextType (vendored in #218, MIT + Commons Clause)
   and re-types when the mode changes.

   WHY IT IS IN THE COMPOSER AT ALL. The reference the owner
   supplied puts a mode picker in the composer's bottom row with
   the mode's meaning reachable from there; this is that meaning,
   rendered. It is deliberately the quietest thing in the
   composer: label-small, `--muted-foreground`, one line.

   THE REDUCED-MOTION BRANCH (hand-written, not inherited).
   When `useReducedMotion()` is true this component renders the
   finished sentence as plain text and DOES NOT MOUNT TextType at
   all — not "renders TextType with motion off". That is the
   stronger guarantee and it is the one worth making: TextType
   is a component with a GSAP cursor tween and a setTimeout
   chain inside it, and the honest way to promise "no timeline
   was created" is never to create the subtree that would own
   one. src/components/Composer.test.tsx asserts the finished
   sentence renders, that the spy records nothing here, and —
   because this component passes `showCursor={false}`, which is
   the one case where "nothing recorded" proves nothing on its
   own — that the same spy DOES record TextType's cursor tween
   when motion is allowed. That positive control is what makes
   the negative assertion mean something.

   `useReducedMotion` fails closed (no matchMedia → true), so an
   environment that cannot answer the question renders still.
   ============================================================ */

import TextType from "./TextType";
import { useReducedMotion } from "../lib/useReducedMotion";
import type { AutonomyMode } from "../lib/tauri";

/** The three modes, in the order the pills list them. Order here
 *  is irrelevant to the animation (one mode is live at a time);
 *  it exists so the lookup cannot return undefined. */
const EXPLAINED_MODES: readonly AutonomyMode[] = [
  "supervised",
  "semi_autonomous",
  "full_autonomous",
];

export interface ComposerAutonomyHintProps {
  mode: AutonomyMode;
  /** Finished sentence for `mode`, already localized. */
  text: string;
  /** Typing speed handed to TextType (ms per character). Slower
   *  than the vendored default because a sentence this long is
   *  read, not watched. */
  typingSpeed?: number;
}

export default function ComposerAutonomyHint({
  mode,
  text,
  typingSpeed = 18,
}: ComposerAutonomyHintProps) {
  const reducedMotion = useReducedMotion();

  // Guards the lookup rather than trusting the caller's union: an
  // unknown mode must degrade to no explainer, never to a crash
  // inside the animation's own frame selection.
  if (!EXPLAINED_MODES.includes(mode)) return null;

  return (
    <span className="nex-autonomy-explain">
      {reducedMotion ? (
        text
      ) : (
        <TextType
          as="span"
          // Keyed on the mode so switching re-types instead of
          // continuing to type a sentence about the old one.
          key={mode}
          text={text}
          showCursor={false}
          loop={false}
          typingSpeed={typingSpeed}
        />
      )}
    </span>
  );
}