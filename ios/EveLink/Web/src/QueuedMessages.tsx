// The owner's sends that wait for eve's reply to end (sendQueue.ts), shown at
// the end of the thread the moment they are sent, oldest first: each is the
// owner's own bubble (UserMessage.tsx), dimmed, with a small "Queued" under
// it. When one starts, OpenUI shows it as an ordinary message and it leaves
// this list in the same update, so it is never on the screen twice.
//
// OpenUI's thread has no slot after its message list, so the list goes into a
// div of ours kept as the scroll area's last child. OpenUI renders exactly
// one child there (the list) and never inserts after it, so the order holds;
// the div scrolls with the thread and sits inside the padding that keeps the
// end clear of the native composer (native.css).

import type { UserMessage } from "@openuidev/react-headless";
import { memo, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ownerParts } from "./attachments";
import type { QueuedSend } from "./sendQueue";
import { OwnerBubble } from "./UserMessage";

const SCROLL_AREA = ".openui-agent-thread-scroll-area";
const BUBBLE = ".openui-shell-thread-message-user__content";

/** One queued send, drawn as the bubble it will become, so nothing moves or restyles when it starts. */
const QueuedItem = memo(function QueuedItem({ item }: { item: QueuedSend }) {
  const message = useMemo(
    () => ({ id: item.key, role: "user", content: ownerParts(item.text, item.attachments) }) as UserMessage,
    [item],
  );
  return (
    <div className="eve-queued__item" role="listitem">
      <OwnerBubble message={message} />
      <span className="eve-queued__label">Queued</span>
    </div>
  );
});

/** The list itself: plain markup, no DOM work (tests render it). */
export function QueuedList({ items }: { items: readonly QueuedSend[] }) {
  if (items.length === 0) return null;
  return (
    <div className="eve-queued" role="list" aria-label="Queued messages">
      {items.map((item) => (
        <QueuedItem key={item.key} item={item} />
      ))}
    </div>
  );
}

export function QueuedMessages({ items }: { items: readonly QueuedSend[] }) {
  const [slot] = useState(() => {
    const el = document.createElement("div");
    el.className = "eve-queued-slot";
    return el;
  });
  const shown = useRef(0);

  // After every render: the slot is the scroll area's last child. OpenUI
  // builds a new scroll area when it changes threads; the slot follows it.
  useLayoutEffect(() => {
    const area = document.querySelector<HTMLElement>(SCROLL_AREA);
    if (area && area.lastElementChild !== slot) area.appendChild(slot);
  });
  useLayoutEffect(() => () => slot.remove(), [slot]);

  // A new queued send was just sent: show it, as a send scrolls to the end.
  useLayoutEffect(() => {
    const grew = items.length > shown.current;
    shown.current = items.length;
    if (!grew) return;
    const area = document.querySelector<HTMLElement>(SCROLL_AREA);
    area?.scrollTo({ top: area.scrollHeight, behavior: "smooth" });
  }, [items.length]);

  // A bubble that wraps takes OpenUI's roomier padding (data-multiline), as
  // OpenUI marks its own bubbles; only inside its list, so ours do it here.
  useLayoutEffect(() => {
    if (items.length === 0 || typeof ResizeObserver === "undefined") return;
    const mark = (bubble: Element) => {
      const style = getComputedStyle(bubble);
      const line = Number.parseFloat(style.lineHeight) || 0;
      const text = bubble.clientHeight - Number.parseFloat(style.paddingTop) - Number.parseFloat(style.paddingBottom);
      bubble.toggleAttribute("data-multiline", line > 0 && text > line * 1.5);
    };
    const observer = new ResizeObserver((entries) => entries.forEach((e) => mark(e.target)));
    slot.querySelectorAll(BUBBLE).forEach((bubble) => {
      mark(bubble);
      observer.observe(bubble);
    });
    return () => observer.disconnect();
  }, [items, slot]);

  return createPortal(<QueuedList items={items} />, slot);
}
