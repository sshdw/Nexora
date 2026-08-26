import NexoraMark from "./NexoraMark";

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
  title = "No conversations yet",
  description = "Your conversations will appear here.",
  actionLabel,
  onAction,
}: EmptyStateProps) {
  return (
    <section className="nex-empty nex-empty-enter" aria-label="Empty state">
      <span className="nex-empty-mark-wrap" aria-hidden="true">
        <NexoraMark className="nex-empty-mark" width={30} height={30} />
      </span>
      <h2 className="nex-empty-title">{title}</h2>
      <p className="nex-empty-text">{description}</p>
      {actionLabel && onAction && (
        <div className="nex-empty-actions">
          <button
            type="button"
            className="nex-btn nex-btn-primary nex-btn-expressive"
            onClick={onAction}
          >
            {actionLabel}
          </button>
        </div>
      )}
    </section>
  );
}
