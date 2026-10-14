// The owner's bubble. A message with photos or files (attachments.ts) shows
// them above the words, right-aligned like the bubble (AttachmentList.tsx: a
// photo as a rounded thumbnail, anything else as a chip with its name; a tap
// opens it full size). A message eve reacted to (reactions.ts) wears the
// reaction as a small badge on the bubble's corner, as in Telegram. Every
// other message is OpenUI's own GenUIUserMessage, unchanged.

import type { UserMessage } from "@openuidev/react-headless";
import { GenUIUserMessage } from "@openuidev/react-ui";
import { memo } from "react";
import { AttachmentList } from "./AttachmentList";
import { attachmentsOf, textOf } from "./attachments";
import { ReactionBadge } from "./Reaction";
import { useReaction } from "./reactions";

// Each bubble carries its message id (data-mid) for Horizontal History's jump back (HorizontalHistory.tsx). The
// marker takes no box of its own, so the bubble lays out exactly as before.
export const EveUserMessage = memo(function EveUserMessage({ message }: { message: UserMessage }) {
  return (
    <div className="eve-mid" data-mid={message.id}>
      <OwnerBubble message={message} />
    </div>
  );
});

const OwnerBubble = memo(function OwnerBubble({ message }: { message: UserMessage }) {
  const reaction = useReaction(message.id);
  const attachments = attachmentsOf(message.content);
  if (attachments.length === 0 && !reaction) return <GenUIUserMessage message={message} />;
  const text = attachments.length > 0 ? textOf(message.content).trim() : "";
  const shell = reaction ? "openui-genui-user-message eve-att-message eve-reacted" : "openui-genui-user-message eve-att-message";
  return (
    <div className="openui-shell-thread-message-user">
      <div className={shell}>
        {attachments.length > 0 && <AttachmentList items={attachments} side="owner" />}
        {attachments.length === 0 ? (
          <GenUIUserMessage message={message} />
        ) : (
          text && <GenUIUserMessage message={{ ...message, content: text }} />
        )}
        {reaction && <ReactionBadge emoji={reaction} />}
      </div>
    </div>
  );
});
