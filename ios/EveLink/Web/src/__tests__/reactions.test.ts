import type { Message } from "@openuidev/react-headless";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { createBridgeLLM } from "../llm";
import { createBridge, type OutboundMessage, type WireMessage } from "../nativeBridge";
import { ReactionBadge } from "../Reaction";
import { MAX_REACTION_CHARS, ReactionStore, sanitizeReaction, useReaction } from "../reactions";
import { Ledger } from "../sync";

const RID = "0f8e6a2c-5b1d-4c3e-9a7f-1e2d3c4b5a69";

function owner(id: string, extra: Partial<WireMessage> = {}): WireMessage {
  return { id, role: "owner", text: "hi", t: 1, via: "app", kind: "say", ...extra };
}

describe("sanitizeReaction", () => {
  it("keeps one short emoji, the receipts eve uses among them", () => {
    for (const e of ["👍", "✍", "🏆", "👀", "👍🏽", "❤️", "👨‍👩‍👧"]) expect(sanitizeReaction(e)).toBe(e);
  });

  it("drops words, markup, spaces, numbers and anything long", () => {
    for (const e of ["ok", "<img src=x onerror=alert(1)>", "👍 ", " ", "1", "#", "中", "", "👍".repeat(9),
      "‍", "️", 42, null, undefined, {}, ["👍"]]) {
      expect(sanitizeReaction(e)).toBe("");
    }
    expect("👍".repeat(9).length).toBeGreaterThan(MAX_REACTION_CHARS);
  });
});

describe("ReactionStore", () => {
  it("shows a history row's reaction on its bubble, and a full set replaces the old one", () => {
    const store = new ReactionStore();
    let heard = 0;
    store.subscribe(() => heard++);
    store.ingest({ messages: [owner("2026-10-09:3", { rid: RID })], reactions: { [RID]: "👀" } });
    expect(store.reactionOf("2026-10-09:3")).toBe("👀");
    store.ingest({ messages: [], reactions: { [RID]: "👍" } });          // replaced
    expect(store.reactionOf("2026-10-09:3")).toBe("👍");
    store.ingest({ messages: [], reactions: { [RID]: "👍" } });          // the same: nobody re-renders
    expect(heard).toBe(2);
    store.ingest({ messages: [], reactions: {} });                       // taken off
    expect(store.reactionOf("2026-10-09:3")).toBe("");
    expect(heard).toBe(3);
  });

  it("ignores a reaction that is not an emoji, a bad rid and an eve row", () => {
    const store = new ReactionStore();
    store.ingest({
      messages: [owner("a:1", { rid: "../x" }), { ...owner("a:2", { rid: RID }), role: "eve" }],
      reactions: { [RID]: "<b>hi</b>", "../x": "👍" },
    });
    expect(store.reactionOf("a:1")).toBe("");
    expect(store.reactionOf("a:2")).toBe("");
  });

  it("falls back to the rows' own reactions from a daemon that sends no set", () => {
    const store = new ReactionStore();
    store.ingest({ messages: [owner("a:1", { rid: RID, reaction: "🏆" })] });
    expect(store.reactionOf("a:1")).toBe("🏆");
  });

  it("links a bubble sent from the page to its client_id, so a live reaction lands on it", async () => {
    const store = new ReactionStore();
    const posted: OutboundMessage[] = [];
    const bridge = createBridge((m) => posted.push(m));
    const llm = createBridgeLLM(bridge, new Ledger({ append: () => {}, lastRole: () => undefined }), store);
    const user = { id: "local-1", role: "user", content: "take notes" } as Message;
    await llm.send({ threadId: "eve", messages: [user], signal: new AbortController().signal } as never);
    const turn = posted.find((m) => m.type === "turn") as Extract<OutboundMessage, { type: "turn" }>;
    expect(turn.client_id).toMatch(/^[A-Za-z0-9_-]{8,64}$/);
    store.ingest({ messages: [], reactions: { [turn.client_id]: "✍" } });
    expect(store.reactionOf("local-1")).toBe("✍");
  });
});

describe("the badge", () => {
  it("draws the emoji in a small badge with a label", () => {
    const html = renderToStaticMarkup(createElement(ReactionBadge, { emoji: "👀" }));
    expect(html).toBe('<span class="eve-reaction" role="img" aria-label="eve reacted 👀">👀</span>');
  });

  it("draws nothing for anything that is not an emoji", () => {
    expect(renderToStaticMarkup(createElement(ReactionBadge, { emoji: "<script>x</script>" }))).toBe("");
    expect(renderToStaticMarkup(createElement(ReactionBadge, { emoji: "" }))).toBe("");
  });

  it("useReaction reads the bubble's reaction from the store", () => {
    const store = new ReactionStore();
    store.ingest({ messages: [owner("a:1", { rid: RID })], reactions: { [RID]: "🏆" } });
    function Probe({ id }: { id: string }) {
      return createElement("i", null, useReaction(id, store));
    }
    expect(renderToStaticMarkup(createElement(Probe, { id: "a:1" }))).toBe("<i>🏆</i>");
    expect(renderToStaticMarkup(createElement(Probe, { id: "a:9" }))).toBe("<i></i>");
  });
});
