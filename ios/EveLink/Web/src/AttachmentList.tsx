// The photos and files on a message, the owner's (right-aligned, above the
// bubble) or eve's own (left-aligned, above its caption): a photo as a
// rounded thumbnail, anything else, or a photo whose upload expired, as a
// chip with its name. A tap asks native to open it full size (Quick Look,
// with share); native fetches it, so the page never touches the network.

import { postToNative, type Post } from "./nativeBridge";
import type { Attachment } from "./attachments";

const ID = /^[0-9a-f]{32}$/;
const GLYPH: Record<Attachment["kind"], string> = {
  image: "Photo",
  pdf: "PDF",
  text: "Text",
  doc: "Document",
  file: "File",
};

let post: Post = postToNative;

/** Tests swap the native post. */
export function setAttachmentPost(p: Post): void {
  post = p;
}

/** Ask native to open one attachment full size → whether it was asked. An expired one has nothing to open. */
export function openAttachment(a: Attachment): boolean {
  if (a.gone || !ID.test(a.id)) return false;
  post({ type: "openAttachment", id: a.id, name: a.name, kind: a.kind });
  return true;
}

function Chip({ a }: { a: Attachment }) {
  return (
    <span className="eve-att-chip">
      <svg className="eve-att-icon" viewBox="0 0 24 24" aria-hidden="true">
        {a.kind === "image" ? (
          <path d="M5 4h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Zm0 2v9.6l3.6-3.6 3 3 4.4-4.4L19 13.6V6H5Z" />
        ) : (
          <path d="M7 2h7l5 5v13a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Zm6 1.5V8h4.5L13 3.5Z" />
        )}
      </svg>
      <span className="eve-att-name">{a.name}</span>
    </span>
  );
}

export function AttachmentList({ items, side }: { items: Attachment[]; side: "owner" | "eve" }) {
  if (items.length === 0) return null;
  const solo = items.length === 1;
  return (
    <div className={side === "eve" ? "eve-attachments eve-attachments--eve" : "eve-attachments"}>
      {items.map((a) => (
        <button
          key={a.id}
          type="button"
          className="eve-att-open"
          disabled={!!a.gone}
          aria-label={a.gone ? `${GLYPH[a.kind]}: ${a.name}, expired` : `Open ${GLYPH[a.kind]}: ${a.name}`}
          onClick={() => openAttachment(a)}
        >
          {a.thumb ? (
            <img
              className={solo ? "eve-att-thumb eve-att-thumb--solo" : "eve-att-thumb"}
              src={a.thumb}
              alt=""
              draggable={false}
            />
          ) : (
            <Chip a={a} />
          )}
        </button>
      ))}
    </div>
  );
}
