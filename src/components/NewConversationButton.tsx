import type { ReactNode } from "react";

import { PlusIcon } from "./icons";

export interface NewConversationButtonProps {
  onClick: () => void;
  disabled?: boolean;
  children?: ReactNode;
}

export default function NewConversationButton({
  onClick,
  disabled = false,
  children = "New Conversation",
}: NewConversationButtonProps) {
  return (
    <button
      type="button"
      className="nex-btn nex-btn-expressive nex-new-conversation"
      onClick={onClick}
      disabled={disabled}
      aria-label="New conversation"
      title="New conversation"
    >
      <PlusIcon className="nex-new-conversation-icon" />
      <span>{children}</span>
    </button>
  );
}


