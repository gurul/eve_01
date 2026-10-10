// Renders one eve reply: markdown parts with OpenUI's MarkDownRenderer and
// OpenUI Lang parts with the react-lang Renderer and openuiChatLibrary.
//
// AgentInterface's built-in GenUI message hands EVERY reply to the Lang
// Renderer when a componentLibrary is set, and a plain markdown reply then
// renders as nothing. eve mostly answers in markdown, so this component
// picks the renderer per part (see replyParts.ts). A picture or file eve
// sent (a "media" line) shows above its caption, left-aligned. A reply that
// leaned on eve's memory shows "Sources" under it (SourceList.tsx).

import type { AssistantMessage } from "@openuidev/react-headless";
import { useThread } from "@openuidev/react-headless";
import type { ActionEvent } from "@openuidev/react-lang";
import { MarkDownRenderer } from "@openuidev/react-ui";
import { memo, useCallback, useMemo, useState } from "react";
import { AttachmentList } from "./AttachmentList";
import { dispatchCardAction } from "./cardActions";
import { GeneratedCard } from "./GeneratedCard";
import { eveMediaOf } from "./attachments";
import { compactLinks, openExternal } from "./links";
import { splitReply } from "./replyParts";
import { SourceList } from "./SourceList";
import { useSources } from "./sources";

function UiPart({
  messageId,
  index,
  code,
  streaming,
}: {
  messageId: string;
  index: number;
  code: string;
  streaming: boolean;
}) {
  const processMessage = useThread((s) => s.processMessage);
  const busy = useThread((s) => s.isRunning);
  const [actionStatus, setActionStatus] = useState("");
  const key = `${messageId}:${index}`;

  const onAction = useCallback((event: ActionEvent) => {
    setActionStatus("");
    void dispatchCardAction(event, processMessage, busy, openExternal).then(setActionStatus);
  }, [processMessage, busy]);

  return <>
    <GeneratedCard key={key} stateKey={key} code={code} streaming={streaming} onAction={onAction} />
    {actionStatus && <p className="eve-generated-card__status" role="status">{actionStatus}</p>}
  </>;
}

export const EveAssistantMessage = memo(function EveAssistantMessage({
  message,
  isStreaming,
}: {
  message: AssistantMessage;
  isStreaming: boolean;
}) {
  const parts = useMemo(
    () => splitReply(message.content ?? "", isStreaming),
    [message.content, isStreaming],
  );
  // eve's own pictures and files (a "media" line, sync.ts), above the caption
  const media = eveMediaOf(message.id);
  // the memory lines it leaned on (sources.ts), under the reply once it is whole
  const sources = useSources(message.id);
  if (parts.length === 0 && media.length === 0) return null;

  return (
    <div className="openui-agent-thread-message-assistant eve-message">
      <div className="openui-agent-thread-message-assistant__content">
        <AttachmentList items={media} side="eve" />
        {parts.map((part, i) =>
          part.kind === "markdown" ? (
            <MarkDownRenderer
              key={i}
              textMarkdown={compactLinks(part.text)}
              className="openui-agent-thread-message-assistant__text"
            />
          ) : (
            <UiPart
              key={i}
              messageId={message.id}
              index={i}
              code={part.code}
              streaming={isStreaming && !part.complete}
            />
          ),
        )}
        {!isStreaming && <SourceList messageId={message.id} items={sources} />}
      </div>
    </div>
  );
});
