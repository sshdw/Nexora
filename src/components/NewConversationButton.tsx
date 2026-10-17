import type { ReactNode } from "react";

import M3Button from "./M3Button";
import { PlusIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface NewConversationButtonProps {
  onClick: () => void;
  disabled?: boolean;
  children?: ReactNode;
}

// The rail's one filled action — the light fill (`--primary`, THEME §1),
// which is also what Composer sends with, so creation reads the same
// everywhere. `h-9` is the 36px standard control (THEME §4 controls) and
// overrides the button's own compact default; the label is what carries the
// action at narrow widths and under reduced motion, so it is never replaced
// by an icon.
//
// New creates NO DB row: the tab opens as a draft and the row appears with
// the first message (NEXORA-DESIGN-SYSTEM.md §4).
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
      block
      className="h-9 justify-center"
      onClick={onClick}
      disabled={disabled}
      aria-label={t("nav.newConversationAria")}
      title={t("nav.newConversationAria")}
    >
      <PlusIcon data-icon="inline-start" className="size-4" />
      <span>{label}</span>
    </M3Button>
  );
}
