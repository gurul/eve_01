// eve's own pictures and files in the chat (owner, 2026-10-09: "It can't
// send attachments" / "Like images"): a "media" transcript line maps to its
// own assistant message with its attachments beside it, is never merged or
// claimed as a copy of the streamed reply, shows after a live turn settles,
// and a tap on any attachment asks native to open it full size.

import type { Message } from "@openuidev/react-headless";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";
import { AttachmentList, openAttachment, setAttachmentPost } from "../AttachmentList";
import { attachmentsOf, eveMediaOf, ownerParts, sanitizeAttachments } from "../attachments";
import { postToNative, type OutboundMessage, type WireMessage } from "../nativeBridge";
import { Ledger, mapLines, type LedgerSink } from "../sync";

const ID1 = "0123456789abcdef0123456789abcdef";
const ID2 = "fedcba9876543210fedcba9876543210";
const THUMB = "data:image/jpeg;base64,/9j/4AAQSkZJRg==";

let n = 0;
const line = (role: WireMessage["role"], text: string, extra: Partial<WireMessage> = {}): WireMessage => ({
  id: `m${++n}`,
  role,
  text,
  t: 5000 + n,
  via: "app",
  kind: "say",
  ...extra,
});

const photo = (caption: string, id = ID1, extra: Partial<WireMessage> = {}) =>
  line("eve", caption, {
    kind: "media",
    attachments: [{ id, name: "001421-0.jpg", kind: "image", thumb: THUMB }],
    ...extra,
  });

const chunk = (content: string) =>
  `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content } }] })}\n\n`;

function sink(canInsert = true) {
  const thread: Message[] = [];
  const state = { idle: true };
  const s: LedgerSink = {
    append: (...m: Message[]) => {
      thread.push(...m);
    },
    lastRole: () => thread.at(-1)?.role,
    idle: () => state.idle,
  };
  // as App.tsx: the reply is taken out and put back after the messages
  if (canInsert)
    s.insertBeforeReply = (...m: Message[]) => {
      const reply = thread.at(-1);
      if (!reply || reply.role !== "assistant" || !thread.some((x) => x.role === "user")) return false;
      thread.splice(thread.length - 1, 1, ...m, reply);
      return true;
    };
  return { thread, state, sink: s };
}

afterEach(() => setAttachmentPost(postToNative));

describe("eve's media lines", () => {
  it("map to their own assistant message with the attachments beside it, even with no caption", () => {
    const p = photo("");
    const msgs = mapLines([line("owner", "take a photo"), p]);
    expect(msgs.map((m) => m.role)).toEqual(["user", "assistant"]);
    expect(msgs[1]).toMatchObject({ id: p.id, content: "" });
    expect(eveMediaOf(p.id)).toEqual([{ id: ID1, name: "001421-0.jpg", kind: "image", thumb: THUMB }]);
  });

  it("are never merged with eve's words before or after, which stay separate bubbles", () => {
    const before = line("eve", "One sec.");
    const p = photo("The desk.");
    const after = line("eve", "Here it is.");
    const msgs = mapLines([before, p, after]);
    expect(msgs.map((m) => m.role)).toEqual(["assistant", "system", "assistant", "system", "assistant"]);
    expect(msgs.filter((m) => m.role === "assistant").map((m) => m.content)).toEqual([
      "One sec.",
      "The desk.",
      "Here it is.",
    ]);
    expect(eveMediaOf(before.id)).toEqual([]);
    expect(eveMediaOf(after.id)).toEqual([]);
  });

  it("an eve line with no words and no attachments is still left out", () => {
    expect(mapLines([line("eve", "  ", { kind: "media" })])).toEqual([]);
  });

  it("an expired picture shows by name and cannot be opened", () => {
    const p = photo("old", ID2, { attachments: [{ id: ID2, name: "x.jpg", kind: "image", gone: true }] });
    mapLines([p]);
    const [a] = eveMediaOf(p.id);
    expect(a).toEqual({ id: ID2, name: "x.jpg", kind: "image", gone: true });
    const posted: OutboundMessage[] = [];
    setAttachmentPost((m) => posted.push(m));
    expect(openAttachment(a!)).toBe(false);
    expect(posted).toEqual([]);
  });

  it("take a file kind, which an owner's upload never has", () => {
    expect(sanitizeAttachments([{ id: ID1, name: "photos.zip", kind: "file" }])).toEqual([
      { id: ID1, name: "photos.zip", kind: "file" },
    ]);
    // round-trips through an owner's binary parts too, distinct from a document
    expect(attachmentsOf(ownerParts("", [{ id: ID1, name: "a.zip", kind: "file" }]))[0]!.kind).toBe("file");
    expect(attachmentsOf(ownerParts("", [{ id: ID1, name: "a.docx", kind: "doc" }]))[0]!.kind).toBe("doc");
  });
});

describe("a live turn", () => {
  it("holds a mid-turn picture and shows it once, before the streamed reply as in history, never claimed", () => {
    const { thread, state, sink: s } = sink();
    const ledger = new Ledger(s);
    thread.push({ id: "ask", role: "user", content: "take a photo" }); // OpenUI's own owner message
    state.idle = false;
    const turn = ledger.beginTurn("take a photo");
    // the caption is the same words as the reply: still not a copy of it
    const owner = line("owner", "take a photo");
    const pic = photo("Here it is.");
    const said = line("eve", "Here it is.");
    ledger.ingest([owner, pic, said]);
    expect(thread.map((m) => m.id)).toEqual(["ask"]);
    turn.bytes(chunk("Here it is."));
    turn.end();
    thread.push({ id: "reply", role: "assistant", content: "Here it is." }); // OpenUI's own streamed reply
    state.idle = true;
    ledger.flushIfIdle();
    expect(thread.map((m) => m.role)).toEqual(["user", "assistant", "system", "assistant"]);
    expect(thread[1]!.id).toBe(pic.id);
    expect(eveMediaOf(pic.id)).toHaveLength(1);
    expect(thread[3]!.id).toBe("reply");
    // the same order as a reload reads it from history
    const history = mapLines([owner, pic, said]);
    expect(history.map((m) => m.role)).toEqual(thread.map((m) => m.role));
    expect(history[1]!.id).toBe(pic.id);
    ledger.ingest([{ ...pic }]);
    expect(thread).toHaveLength(4); // the same id again is never shown twice
  });

  it("a picture after the reply's own line, or once the stream ended, goes after the reply", () => {
    const { thread, state, sink: s } = sink();
    const ledger = new Ledger(s);
    thread.push({ id: "ask", role: "user", content: "hi" });
    state.idle = false;
    const turn = ledger.beginTurn("hi");
    turn.bytes(chunk("Hello."));
    ledger.ingest([line("owner", "hi"), line("eve", "Hello."), photo("a task's screen", ID2)]);
    turn.end();
    const late = photo("the chief's picture", ID1);
    ledger.ingest([late]); // the stream had ended: the reply's line is already written
    thread.push({ id: "reply", role: "assistant", content: "Hello." });
    state.idle = true;
    ledger.flushIfIdle();
    expect(thread.map((m) => m.id).filter((id) => !id.startsWith("sep:"))).toEqual([
      "ask",
      "reply",
      expect.any(String),
      late.id,
    ]);
  });

  it("without insertBeforeReply, a held picture is appended after the reply as before", () => {
    const { thread, state, sink: s } = sink(false);
    const ledger = new Ledger(s);
    state.idle = false;
    const turn = ledger.beginTurn("take a photo");
    ledger.ingest([line("owner", "take a photo"), photo("Here it is."), line("eve", "Here it is.")]);
    turn.bytes(chunk("Here it is."));
    turn.end();
    thread.push({ id: "reply", role: "assistant", content: "Here it is." });
    state.idle = true;
    ledger.flushIfIdle();
    expect(thread.map((m) => m.role)).toEqual(["assistant", "system", "assistant"]);
    expect(eveMediaOf(thread[2]!.id)).toHaveLength(1);
  });

  it("back to back (a queued message started as the reply ended): a late picture of the turn before never goes before the new reply", () => {
    const { thread, state, sink: s } = sink();
    const ledger = new Ledger(s);
    // turn A, and the poll's copy of its owner line, read while it streams
    thread.push({ id: "askA", role: "user", content: "take a photo" });
    state.idle = false;
    const a = ledger.beginTurn("take a photo", "askA");
    const ownerA = line("owner", "take a photo");
    ledger.ingest([ownerA]);
    a.bytes(chunk("Here it is."));
    a.end();
    thread.push({ id: "replyA", role: "assistant", content: "Here it is." });
    state.idle = true;
    ledger.flushIfIdle();
    // the queued message starts at once: turn B streams
    thread.push({ id: "askB", role: "user", content: "thanks" });
    state.idle = false;
    const b = ledger.beginTurn("thanks", "askB");
    // only now does the poll bring A's last picture (before B's owner line in the transcript)
    const lateA = photo("the photo", ID1);
    ledger.ingest([lateA]);
    b.bytes(chunk("Anytime."));
    // B's own picture, after B's owner line: before B's reply, as for a single turn
    const ownerB = line("owner", "thanks");
    const picB = photo("a second one", ID2);
    ledger.ingest([ownerB, picB]);
    b.end();
    thread.push({ id: "replyB", role: "assistant", content: "Anytime." });
    state.idle = true;
    ledger.flushIfIdle();
    const ids = thread.map((m) => m.id).filter((id) => !id.startsWith("sep:") && !id.startsWith("reply:"));
    expect(ids).toEqual(["askA", "replyA", "askB", picB.id, "replyB", lateA.id]);
  });

  it("a picture sent while nothing is streaming (a task, the chief) shows at once", () => {
    const { thread, sink: s } = sink();
    const ledger = new Ledger(s);
    ledger.ingest([photo("the screen when the task ended", ID2, { via: "telegram" })]);
    expect(thread.map((m) => m.role)).toEqual(["assistant"]);
    expect(eveMediaOf(thread[0]!.id)[0]!.id).toBe(ID2);
  });
});

describe("opening full size", () => {
  it("asks native with the id, name and kind, and never for a malformed id", () => {
    const posted: OutboundMessage[] = [];
    setAttachmentPost((m) => posted.push(m));
    expect(openAttachment({ id: ID1, name: "lease.pdf", kind: "pdf" })).toBe(true);
    expect(openAttachment({ id: "../etc/passwd", name: "x", kind: "pdf" })).toBe(false);
    expect(posted).toEqual([{ type: "openAttachment", id: ID1, name: "lease.pdf", kind: "pdf" }]);
  });

  it("draws eve's attachments left-aligned as buttons: a thumbnail for a photo, a chip for a file", () => {
    const html = renderToStaticMarkup(
      createElement(AttachmentList, {
        side: "eve",
        items: [
          { id: ID1, name: "desk.jpg", kind: "image", thumb: THUMB },
          { id: ID2, name: "lease.pdf", kind: "pdf" },
        ],
      }),
    );
    expect(html).toContain('class="eve-attachments eve-attachments--eve"');
    expect(html.match(/<button/g)).toHaveLength(2);
    expect(html).toContain(`src="${THUMB}"`);
    expect(html).toContain("lease.pdf");
    expect(html).toContain('aria-label="Open PDF: lease.pdf"');
    const owner = renderToStaticMarkup(
      createElement(AttachmentList, { side: "owner", items: [{ id: ID1, name: "a.jpg", kind: "image", gone: true }] }),
    );
    expect(owner).toContain('class="eve-attachments"');
    expect(owner).toContain("disabled");
    expect(renderToStaticMarkup(createElement(AttachmentList, { side: "eve", items: [] }))).toBe("");
  });
});
