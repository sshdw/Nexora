import type { ReactNode } from "react";

import M3Button from "./M3Button";
import { PlusIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface NewConversationButtonProps {
  onClick: () => void;
  disabled?: boolean;
  children?: ReactNode;
}

export default function NewConversationButton({
  onClick,
  disabled = false,
  children,
}: NewConversationButtonProps) {
  const { t } = useStrings();
  const label = children ?? t("nav.newConversation");
  return (
    <M3Button
      variant="primary"
      expressive
      block
      className="nex-new-conversation"
      onClick={onClick}
      disabled={disabled}
      aria-label={t("nav.newConversationAria")}
      title={t("nav.newConversationAria")}
    >
      <PlusIcon className="nex-new-conversation-icon" />
      <span>{label}</span>
    </M3Button>
  );
}
