/* ============================================================
   Autonomy explainer — the composer's one line (#220, re-based #230 P4)

   WHAT IT IS. One dim sentence under the autonomy pills that says
   what the selected mode actually does — the mode names
   ("Supervised", "Semi-auto", "Full-auto") are labels nobody can
   act on without that sentence.

   PLAIN TEXT ALWAYS. The per-character TextType typing is
   ornament on a static sentence, so it is removed: switching
   modes swaps the sentence instantly, with no re-typing
   animation, and there is no JS timeline to collapse under
   reduced motion. (`TextType` stays vendored for genuine
   streaming/progress semantics elsewhere; it is not used here.)

   One line, clamped with `title=` fallback so the full sentence
   is still reachable on hover/focus when it overflows.
   ============================================================ */

import type { AutonomyMode } from "../lib/tauri";

/** The three modes, in the order the pills list them. Order here
 *  is irrelevant to the explainer (one mode is live at a time);
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
}

export default function ComposerAutonomyHint({ mode, text }: ComposerAutonomyHintProps) {
  // Guards the lookup rather than trusting the caller's union: an
  // unknown mode must degrade to no explainer, never to a crash.
  if (!EXPLAINED_MODES.includes(mode)) return null;

  return <span className="nex-autonomy-explain" title={text}>
    {text}
  </span>;
}
