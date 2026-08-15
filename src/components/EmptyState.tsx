import NexoraMark from "./NexoraMark";

export interface EmptyStateProps {
  title?: string;
  description?: string;
}

export default function EmptyState({
  title = "No conversations yet",
  description = "Your conversations will appear here.",
}: EmptyStateProps) {
  return (
    <section className="nex-empty" aria-label="Empty state">
      <NexoraMark className="nex-empty-mark" width={28} height={28} />
      <h2 className="nex-empty-title">{title}</h2>
      <p className="nex-empty-text">{description}</p>
      <p className="nex-empty-prompt">
        Use New Conversation in the sidebar to get started.
      </p>
    </section>
  );
}
