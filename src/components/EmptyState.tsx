import M3Button from "./M3Button";
import NexoraMark from "./NexoraMark";
import { useStrings } from "../lib/useLocale";

export interface EmptyStateProps {
  title?: string;
  description?: string;
  /** Optional primary action rendered under the copy. 0.3.0 re-exposes
   * the existing "New Conversation" function so the empty workspace
   * composes instead of floating in whitespace (visual only). */
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
      <span className="nex-empty-mark-wrap" aria-hidden="true">
        <NexoraMark className="nex-empty-mark" width={30} height={30} />
      </span>
      <h2 className="nex-empty-title">{title ?? t("empty.title")}</h2>
      <p className="nex-empty-text">{description ?? t("empty.text")}</p>
      {actionLabel && onAction && (
        <div className="nex-empty-actions">
          <M3Button variant="primary" expressive onClick={onAction}>
            {actionLabel}
          </M3Button>
        </div>
      )}
    </section>
  );
}
