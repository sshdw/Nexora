import type { ReactNode } from "react";

import M3Button from "./M3Button";
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
    <M3Button
      variant="primary"
      expressive
      block
      className="nex-new-conversation"
      onClick={onClick}
      disabled={disabled}
      aria-label="New conversation"
      title="New conversation"
    >
      <PlusIcon className="nex-new-conversation-icon" />
      <span>{children}</span>
    </M3Button>
  );
}
