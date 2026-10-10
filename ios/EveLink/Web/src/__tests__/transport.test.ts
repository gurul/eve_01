import { EventType, openAIAdapter, type AGUIEvent } from "@openuidev/react-headless";
import { describe, expect, it, vi } from "vitest";
import { createBridgeLLM } from "../llm";
import { createBridge, type OutboundMessage } from "../nativeBridge";
import { LIVE_MARK, splitReply } from "../replyParts";
import { Ledger } from "../sync";

const chunk = (content: string) =>
  `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content } }] })}\n\n`;

function setup() {
  const posts: OutboundMessage[] = [];
  const bridge = createBridge((m) => posts.push(m));
  return { posts, bridge };
}

async function collect(events: AsyncIterable<AGUIEvent>): Promise<AGUIEvent[]> {
  const out: AGUIEvent[] = [];
  for await (const e of events) out.push(e);
  return out;
}

function turnId(posts: OutboundMessage[]): string {
  const turn = posts.find((p) => p.type === "turn");
  if (!turn || turn.type !== "turn") throw new Error("no turn posted");
  return turn.id;
}

describe("bridge transport", () => {
  it("posts a turn and turns native chunks into openAIAdapter events", async () => {
    const { posts, bridge } = setup();
    const ac = new AbortController();
    const response = bridge.openTurn({ text: "hi eve", clientId: "c-1", signal: ac.signal });

    const turn = posts[0];
    expect(turn).toMatchObject({ type: "turn", text: "hi eve", client_id: "c-1" });
    const id = turnId(posts);

    const events = collect(openAIAdapter().parse(response));
    const wire = chunk("Hello") + ": ping\n\n" + chunk("\n\nworld") + "data: [DONE]\n\n";
    // Split mid-line to prove partial SSE lines are reassembled.
    bridge.native.onChunk(id, wire.slice(0, 17));
    bridge.native.onChunk(id, wire.slice(17, 60));
    bridge.native.onChunk(id, wire.slice(60));
    bridge.native.onDone(id);

    const got = await events;
    expect(got[0]).toMatchObject({ type: EventType.TEXT_MESSAGE_START, role: "assistant" });
    const text = got
      .filter((e) => e.type === EventType.TEXT_MESSAGE_CONTENT)
      .map((e) => (e as { delta: string }).delta)
      .join("");
    expect(text).toBe("Hello\n\nworld");
    expect(response.headers.get("content-type")).toBe("text/event-stream");
  });

  it("cancels on the AbortSignal: posts cancel, closes the stream, ignores late chunks", async () => {
    const { posts, bridge } = setup();
    const ac = new AbortController();
    const onEnd = vi.fn();
    const response = bridge.openTurn({ text: "x", clientId: "c", signal: ac.signal, hooks: { onEnd } });
    const id = turnId(posts);
    const events = collect(openAIAdapter().parse(response));

    bridge.native.onChunk(id, chunk("partial"));
    ac.abort();
    bridge.native.onChunk(id, chunk("late"));

    const got = await events;
    expect(posts).toContainEqual({ type: "cancel", id });
    const text = got.filter((e) => e.type === EventType.TEXT_MESSAGE_CONTENT).map((e) => (e as { delta: string }).delta);
    expect(text).toEqual(["partial"]);
    expect(onEnd).toHaveBeenCalledTimes(1);
  });

  it("does not post a turn when the signal is already aborted", () => {
    const { posts, bridge } = setup();
    const ac = new AbortController();
    ac.abort();
    bridge.openTurn({ text: "x", clientId: "c", signal: ac.signal });
    expect(posts.some((p) => p.type === "turn")).toBe(false);
  });

  it("errors the stream on onError so the run fails with native's message", async () => {
    const { posts, bridge } = setup();
    const response = bridge.openTurn({ text: "x", clientId: "c", signal: new AbortController().signal });
    const id = turnId(posts);
    const events = collect(openAIAdapter().parse(response));
    bridge.native.onChunk(id, chunk("a"));
    bridge.native.onError(id, "Eve's Mac is asleep");
    await expect(events).rejects.toThrow("Eve's Mac is asleep");
  });

  it("ChatLLM: no send haptic (native owns it), soft once on the first words, never on a ping", async () => {
    const { posts, bridge } = setup();
    const appended: unknown[] = [];
    const ledger = new Ledger({ append: (...m) => appended.push(...m), lastRole: () => undefined });
    const llm = createBridgeLLM(bridge, ledger);

    const response = await llm.send({
      threadId: "eve",
      messages: [{ id: "u1", role: "user", content: "  what's up  " }],
      signal: new AbortController().signal,
    });
    expect(posts[0]).toMatchObject({ type: "turn", text: "what's up" });
    expect(posts.filter((p) => p.type === "haptic")).toHaveLength(0);
    const id = turnId(posts);
    const events = collect(llm.streamProtocol.parse(response));
    bridge.native.onChunk(id, ": ping\n\n"); // 15 s in, no words yet
    expect(posts.filter((p) => p.type === "haptic")).toHaveLength(0);
    bridge.native.onChunk(id, chunk("one"));
    bridge.native.onChunk(id, chunk(" two"));
    expect(posts.filter((p) => p.type === "haptic")).toEqual([{ type: "haptic", style: "soft" }]);
    expect(ledger.streaming).toBe(true);
    bridge.native.onDone(id);
    await events;
    expect(ledger.streaming).toBe(false);
  });

  it("a live markdown reply carries the mark OpenUI needs to show it while it streams", async () => {
    // OpenUI 0.17.0 InterleavedTurn shows a live turn's message only when its
    // text has "```openui-lang" or a `root =` line (hasLangSyntax).
    const { posts, bridge } = setup();
    const llm = createBridgeLLM(bridge, new Ledger({ append: () => {}, lastRole: () => undefined }));
    const response = await llm.send({
      threadId: "eve",
      messages: [{ id: "u1", role: "user", content: "hi" }],
      signal: new AbortController().signal,
    });
    const id = turnId(posts);
    const seen: string[] = [];
    const reading = (async () => {
      let content = "";
      for await (const e of llm.streamProtocol.parse(response)) {
        if (e.type === EventType.TEXT_MESSAGE_CONTENT) {
          content += e.delta;
          seen.push(content);
        }
      }
    })();
    bridge.native.onChunk(id, chunk("Sure, **here** it is."));
    bridge.native.onChunk(id, chunk(" More."));
    await new Promise((r) => setTimeout(r, 0));
    // Before onDone: the text so far already passes OpenUI's gate...
    expect(seen.length).toBeGreaterThan(0);
    for (const content of seen) expect(content.includes("```openui-lang")).toBe(true);
    // ...and renders as the markdown alone, the mark dropped.
    expect(splitReply(seen.at(-1)!, true)).toEqual([{ kind: "markdown", text: "Sure, **here** it is. More." }]);
    bridge.native.onDone(id);
    await reading;
    expect(seen.at(-1)).toBe(LIVE_MARK + "Sure, **here** it is. More.");
  });

  it("client_id is a fresh id of at most 64 chars per turn", async () => {
    const { posts, bridge } = setup();
    const ledger = new Ledger({ append: () => {}, lastRole: () => undefined });
    const llm = createBridgeLLM(bridge, ledger);
    for (let i = 0; i < 2; i++) {
      await llm.send({
        threadId: "eve",
        messages: [{ id: `u${i}`, role: "user", content: "hi" }],
        signal: new AbortController().signal,
      });
    }
    const ids = posts.flatMap((p) => (p.type === "turn" ? [p.client_id] : []));
    expect(ids).toHaveLength(2);
    expect(ids[0]).not.toBe(ids[1]);
    for (const c of ids) expect(c.length).toBeLessThanOrEqual(64);
  });
});

describe("bridge messages + native calls", () => {
  it("requestMessages posts after/wait and resolves from onMessages", async () => {
    const { posts, bridge } = setup();
    const p = bridge.requestMessages("c9", 25);
    const msg = posts[0];
    expect(msg).toMatchObject({ type: "messages", after: "c9", wait: 25 });
    if (msg?.type !== "messages") throw new Error("expected messages");
    bridge.native.onMessages(
      msg.id,
      JSON.stringify({
        messages: [{ id: "1", role: "eve", text: "hey", t: 1, via: "telegram", kind: "say" }],
        cursor: "c10",
        busy: true,
      }),
    );
    await expect(p).resolves.toEqual({
      messages: [{ id: "1", role: "eve", text: "hey", t: 1, via: "telegram", kind: "say" }],
      cursor: "c10",
      busy: true,
    });
  });

  it("history request omits after", () => {
    const { posts, bridge } = setup();
    void bridge.requestMessages(undefined, 0).catch(() => {});
    expect(posts[0]).not.toHaveProperty("after");
  });

  it("requestMessages rejects on onError and times out with a cancel", async () => {
    vi.useFakeTimers();
    try {
      const { posts, bridge } = setup();
      const failed = bridge.requestMessages("a", 0);
      const first = posts[0];
      if (first?.type !== "messages") throw new Error("expected messages");
      bridge.native.onError(first.id, "503");
      await expect(failed).rejects.toThrow("503");

      const slow = bridge.requestMessages("a", 25, 1000);
      const second = posts[1];
      if (second?.type !== "messages") throw new Error("expected messages");
      const assertion = expect(slow).rejects.toThrow("timed out");
      await vi.advanceTimersByTimeAsync(1001);
      await assertion;
      expect(posts).toContainEqual({ type: "cancel", id: second.id });
    } finally {
      vi.useRealTimers();
    }
  });

  it("queues native send() until a handler is set, then delivers in order", () => {
    const { bridge } = setup();
    bridge.native.send("first");
    bridge.native.send("   ");
    bridge.native.send("second");
    const got: string[] = [];
    bridge.setSendHandler((t) => got.push(t));
    bridge.native.send("third");
    expect(got).toEqual(["first", "second", "third"]);
  });

  it("setBusy and scrollToBottom reach their listeners", () => {
    const { bridge } = setup();
    const busy: boolean[] = [];
    const off = bridge.onBusy((b) => busy.push(b));
    bridge.native.setBusy(true);
    off();
    bridge.native.setBusy(false);
    expect(busy).toEqual([true]);
    const scroll = vi.fn();
    bridge.setScrollHandler(scroll);
    bridge.native.scrollToBottom();
    expect(scroll).toHaveBeenCalledOnce();
  });
});
