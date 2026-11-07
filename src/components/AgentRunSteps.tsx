//! Steps accordion for one agent run (Task 5.1 + 5.2).
//!
//! Presentational: kind-grouped collapsible sections (`model_turn` |
//! `tool_call` | `approval`), run status pill, live append, bare inline
//! Approve/Deny on `ApprovalRequested`, Continue on `BudgetExhausted`, pause/
//! resume controls, and per-tool body renderers — terminal for
//! `execute_command` (command line + stdout/stderr with distinct separator) and
//! diff viewer for `write_file` (headers, hunks, +/- gutters, mono,
//! collapsible). Chronological placement is owned by the thread view.
//!
//! #153 — Agent Activity Spine + run-state visuals (brief §7, §13.9, §13.33,
//! §15, §16, §18, §19):
//!   - ONE always-mounted hairline rail per run, with a per-row connector
//!     tick. The rail is the structural boundary; the tick is a segment, not
//!     a per-row border — nothing here rebuilds a spine out of row borders.
//!   - The travelling accent pulse is a CSS animation keyed on the run-status
//!     class on the section. Never a timer: every step frame replaces the
//!     runs array and re-renders this whole subtree, so a JS pulse keyed on
//!     step arrival would restart or retime on every frame.
//!   - The pulse exists ONLY while the run is actually executing. Idle,
//!     `paused`, every terminal, and a run parked on an approval carry the
//!     same information with no motion at all, and the reduced-motion frame
//!     is a static accent segment, not an empty rail (brief §15:885-887).
//!   - Temporal fade (§13.9): inactive rows step their ink down the
//!     on-surface ladder and stop at `on-surface-tertiary`. No opacity
//!     multiplier — see the CSS note; the floor tier has no headroom for one.
//!   - Tool group folding (§13.33): a finished, approval-free, error-free,
//!     long sequence collapses behind one disclosure. "Error-free" is
//!     the persisted per-step vocabulary — `failed`, `denied` or `cancelled`
//!     (`isFailedStatus`) — and "approval-free" means no approval still
//!     OUTSTANDING: a granted park is settled history. Expanding
//!     restores the SAME step objects in the SAME order with each row's own
//!     open/closed state intact (rows are hidden, never unmounted).
//!
//! #230 Phase 3 (MAP rows 8 + 9, SYSTEM §7 + §8):
//!   - ONE SUMMARY LINE. The header carries the status word and the run's
//!     real results, counted from its own steps, exactly once. The old
//!     counted fold summary was a SECOND block claiming the same outcome,
//!     which P3's acceptance forbids outright.
//!   - THE STEP GRAMMAR: action → file → change → verification, one line.
//!     Past-tense verbs instead of tool identifiers, the acted-on path with
//!     its full value on hover, `+N −M` counted from the diff text itself,
//!     and the persisted status as a word rather than a CHECK-constraint
//!     string. Never colour-only.
//!   - REPEATED CALLS GROUP. A consecutive run of the same tool against the
//!     same target is one fact ("Read ×6 src/db.rs"), behind a disclosure
//!     that restores the same rows in the same order. Interleaved work keeps
//!     its real order — the timeline never lies about sequence.
//!   - THE ERROR BLOCK. What happened / what survived / next action, in
//!     words, outside the timeline so the fold and the temporal fade can
//!     never take it out of reach. The old bare `nowrap` error string in the
//!     toolbar clipped long provider errors and answered nothing else.
//!   - NO MODEL MONOLOGUE. `model_turn` rows read "Thought" and stay folded
//!     until asked; the raw narration opens on request rather than streaming
//!     into the thread.
//!
//! Callbacks arrive pre-bound to their run id and stable, so the memoized
//! root re-renders only when this run's own data changes (#148).
//!
//! AC-208 (review §3.1): this file is the public barrel of the split —
//! the root component, the step sections and the shared helpers live in
//! `runSteps/` (labels, grouping, StepSection, StepGroupSection), and
//! every historical export (default component, `AgentRunStepsProps`,
//! `DiffView`, `diffCounts`) survives unchanged so no consumer churns.
//! (The folder is `runSteps/`, not `agentRunSteps/`: on Windows'
//! case-insensitive filesystem the plan's original name would collide with
//! this parent file, whose import specifier must stay `./AgentRunSteps`.)

export {
  default,
  type AgentRunStepsProps,
  diffCounts,
  DiffView,
} from "./runSteps";
