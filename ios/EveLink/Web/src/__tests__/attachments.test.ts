import type { Message } from "@openuidev/react-headless";
import { describe, expect, it } from "vitest";
import {
  attachmentIds,
  attachmentsOf,
  MAX_ATTACHMENTS,
  ownerParts,
  parseNativeAttachments,
  sanitizeAttachments,
  textOf,
} from "../attachments";
import { createBridgeLLM } from "../llm";
import { createBridge, type OutboundMessage, type WireMessage } from "../nativeBridge";
import { ownerText } from "../replyParts";
import { Ledger, mapLines } from "../sync";

const ID1 = "0123456789abcdef0123456789abcdef";
const ID2 = "fedcba9876543210fedcba9876543210";
const THUMB = "data:image/jpeg;base64,/9j/4AAQSkZJRg==";

describe("sanitizeAttachments", () => {
  it("keeps well-formed entries and drops the rest", () => {
    const got = sanitizeAttachments([
      { id: ID1, name: "beach.jpg", kind: "image", thumb: THUMB },
      { id: "../etc", name: "x", kind: "image" },
      { id: ID2, name: "x", kind: "exe" },
      { id: ID2, name: "lease.pdf", kind: "pdf" },
      "junk",
    ]);
    expect(got).toEqual([
      { id: ID1, name: "beach.jpg", kind: "image", thumb: THUMB },
      { id: ID2, name: "lease.pdf", kind: "pdf" },
    ]);
  });

  it("takes a thumbnail only as an inline JPEG or PNG, never a URL to fetch", () => {
    for (const thumb of ["https://evil.example/x.jpg", "javascript:alert(1)", "data:text/html;base64,PGI+",
      "data:image/svg+xml;base64,PHN2Zz4=", `${THUMB}"onerror="x`]) {
      expect(sanitizeAttachments([{ id: ID1, name: "a", kind: "image", thumb }])[0]!.thumb).toBeUndefined();
    }
    expect(sanitizeAttachments([{ id: ID1, name: "a", kind: "pdf", thumb: THUMB }])[0]!.thumb).toBeUndefined();
  });

  it("caps the count and names an unnamed photo", () => {
    const many = Array.from({ length: 9 }, (_, i) => ({ id: i.toString(16).repeat(32), name: "", kind: "image" }));
    const got = sanitizeAttachments(many);
    expect(got).toHaveLength(MAX_ATTACHMENTS);
    expect(got[0]!.name).toBe("Photo");
  });

  it("parses native's JSON and ignores anything malformed", () => {
    expect(parseNativeAttachments(JSON.stringify([{ id: ID1, name: "a.pdf", kind: "pdf" }]))).toHaveLength(1);
    expect(parseNativeAttachments("{not json")).toEqual([]);
    expect(parseNativeAttachments(undefined)).toEqual([]);
  });
});

describe("owner content", () => {
  it("is the plain text without attachments, and text plus binary parts with them", () => {
    expect(ownerParts("hi", [])).toBe("hi");
    const content = ownerParts("look", [
      { id: ID1, name: "beach.jpg", kind: "image", thumb: THUMB },
      { id: ID2, name: "lease.pdf", kind: "pdf" },
    ]);
    expect(attachmentsOf(content)).toEqual([
      { id: ID1, name: "beach.jpg", kind: "image", thumb: THUMB },
      { id: ID2, name: "lease.pdf", kind: "pdf" },
    ]);
    expect(attachmentIds(content)).toEqual([ID1, ID2]);
    expect(textOf(content)).toBe("look");
    expect(ownerText(content)).toBe("look"); // the daemon gets the words; the ids go separately
  });
});

describe("the bridge", () => {
  it("native's send carries the attachments to the handler, and an empty text is fine with one", () => {
    const bridge = createBridge(() => {});
    const got: [string, number][] = [];
    bridge.native.send("", JSON.stringify([{ id: ID1, name: "a.jpg", kind: "image" }])); // queued before ready
    bridge.native.send("", "[]"); // nothing to send: dropped
    bridge.setSendHandler((text, atts) => got.push([text, atts.length]));
    bridge.native.send("and this", JSON.stringify([{ id: ID2, name: "b.pdf", kind: "pdf" }]));
    bridge.native.send("plain");
    expect(got).toEqual([
      ["", 1],
      ["and this", 1],
      ["plain", 0],
    ]);
  });

  it("a turn posts its upload ids; one without attachments posts none", async () => {
    const posts: OutboundMessage[] = [];
    const bridge = createBridge((m) => posts.push(m));
    const llm = createBridgeLLM(bridge, new Ledger({ append: () => {}, lastRole: () => undefined }));
    const ac = new AbortController();
    const content = ownerParts("what is this?", [{ id: ID1, name: "a.jpg", kind: "image", thumb: THUMB }]);
    await llm.send({ messages: [{ id: "u1", role: "user", content } as Message], signal: ac.signal } as never);
    await llm.send({ messages: [{ id: "u2", role: "user", content: "plain" } as Message], signal: ac.signal } as never);
    const turns = posts.filter((p) => p.type === "turn");
    expect(turns[0]).toMatchObject({ type: "turn", text: "what is this?", attachments: [ID1] });
    expect(turns[1]).not.toHaveProperty("attachments");
    ac.abort();
  });
});

describe("history", () => {
  const owner = (text: string, attachments: unknown): WireMessage => ({
    id: "d:1",
    role: "owner",
    text,
    t: 1,
    via: "app",
    kind: "image",
    attachments,
  });

  it("an owner line with attachments shows them, even with no words", () => {
    const [msg] = mapLines([owner("", [{ id: ID1, name: "a.jpg", kind: "image", thumb: THUMB }])]);
    expect(msg!.role).toBe("user");
    expect(attachmentsOf(msg!.content)).toEqual([{ id: ID1, name: "a.jpg", kind: "image", thumb: THUMB }]);
  });

  it("an expired photo shows as its name, without the picture", () => {
    const [msg] = mapLines([owner("hi", [{ id: ID1, name: "a.jpg", kind: "image", thumb: THUMB, gone: true }])]);
    expect(attachmentsOf(msg!.content)).toEqual([{ id: ID1, name: "a.jpg", kind: "image" }]);
  });

  it("an empty owner line without attachments is still left out (control)", () => {
    expect(mapLines([owner("", undefined)])).toEqual([]);
  });

  it("the ledger claims the owner line of an app turn with only a photo", () => {
    const thread: Message[] = [];
    const ledger = new Ledger({ append: (...m) => thread.push(...m), lastRole: () => thread.at(-1)?.role });
    const claim = ledger.beginTurn("");
    claim.end();
    ledger.ingest([owner("", [{ id: ID1, name: "a.jpg", kind: "image" }])]);
    expect(thread).toEqual([]);
  });
});
