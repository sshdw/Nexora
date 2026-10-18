//! Empty state — COMPACT, one action max (#230 Phase 3, `empty.md`).
//!
//! `empty.md` composes media + title + description + actions. The mono
//! theme spends none of them by default:
//!
//!   · THE MARK IS GONE. A 64px logo plate with a border is a hero, and a
//!     hero is what THEME §6's rejected list calls a permanent-attention
//!     thing. The state says what happened in two sentences; that is
//!     enough, and the sidebar's own affordances are right there.
//!   · THE HEADING IS NOT A DISPLAY BAND. `--nex-sys-type-size-display-small`
//!     was reserved for "once per app"; an empty state that can appear in
//!     every pane was spending it by the pane. It is now the standard
//!     medium weight, which is THEME §6's "chrome 13/500".
//!   · THE COPY CLAMPS. A long description in an unbounded empty state
//!     grows the pane instead of filling it; three lines is enough for any
//!     of the two strings the catalog holds.
//!
//! ONE ACTION MAX is structural, not a convention: `actionLabel` is a
//! single string, so there is no way to pass a second action. The
//! conversation pane passes none at all — its one action is the composer
//! already on screen, and a button saying "write a message" next to the
//! box that takes a message is the duplication THEME §6 rejects.

import M3Button from "./M3Button";
import { useStrings } from "../lib/useLocale";

export interface EmptyStateProps {
  title?: string;
  description?: string;
  /** THE action. Single by construction — see the note above. */
  actionLabel?: string;
  onAction?: () => void;
}

export default function EmptyState({
  title,
  description,
  actionLabel,
  onAction,
}: EmptyStateProps) {
  const { t } = useStrings();
  return (
    <section className="nex-empty nex-empty-enter" aria-label={t("empty.aria")}>
      <h2 className="nex-empty-title">{title ?? t("empty.title")}</h2>
      <p className="nex-empty-text">{description ?? t("empty.text")}</p>
      {actionLabel && onAction && (
        <div className="nex-empty-actions">
          <M3Button variant="primary" onClick={onAction}>
            {actionLabel}
          </M3Button>
        </div>
      )}
    </section>
  );
}