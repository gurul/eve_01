import type { Message } from "@openuidev/react-headless";
import { describe, expect, it } from "vitest";
import { createBridge, type OutboundMessage, type WireMessage } from "../nativeBridge";
import { createEveStorage, startLivePoll } from "../storage";
import { Ledger, mapLines, MERGE_WINDOW_S } from "../sync";

let n = 0;
const line = (role: WireMessage["role"], text: string, extra: Partial<WireMessage> = {}): WireMessage => ({
  id: `l${++n}`,
  role,
  text,
  t: 1000 + n,
  via: "app",
  kind: "say",
  ...extra,
});

const chunk = (content: string) =>
  `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content } }] })}\n\n`;

function sink() {
  const thread: Message[] = [];
  const state = { idle: true };
  return {
    thread,
    state,
    sink: {
      append: (...m: Message[]) => thread.push(...m),
      lastRole: () => thread.at(-1)?.role,
      idle: () => state.idle,
    },
  };
}

const visibleText = (thread: Message[]) =>
  thread.filter((m) => m.role !== "system").map((m) => `${m.role}:${m.content as string}`);

describe("mapLines", () => {
  it("maps owner -> user and eve -> assistant, oldest first, keeping ids", () => {
    const a = line("owner", "hi");
    const b = line("eve", "hello");
    expect(mapLines([a, b])).toEqual([
      { id: a.id, role: "user", content: "hi" },
      { id: b.id, role: "assistant", content: "hello" },
    ]);
  });

  it("leaves out tool lines and empty text", () => {
    const out = mapLines([line("eve", "ran ls", { kind: "tool" }), line("eve", "   "), line("owner", "ok")]);
    expect(visibleText(out)).toEqual(["user:ok"]);
  });

  it("merges consecutive eve lines inside the window like a streamed turn", () => {
    const a = line("eve", "one", { t: 100 });
    const b = line("eve", "two", { t: 100 + MERGE_WINDOW_S });
    const out = mapLines([a, b]);
    expect(out).toEqual([{ id: a.id, role: "assistant", content: "one\n\ntwo" }]);
  });

  it("keeps eve lines far apart as separate bubbles with a hidden separator", () => {
    const a = line("eve", "morning", { t: 100 });
    const b = line("eve", "reminder", { t: 100 + MERGE_WINDOW_S + 1 });
    const out = mapLines([a, b]);
    expect(out.map((m) => m.role)).toEqual(["assistant", "system", "assistant"]);
  });

  it("adds a separator when the thread already ends with eve", () => {
    const out = mapLines([line("eve", "later")], { id: "x", role: "assistant", content: "earlier" });
    expect(out.map((m) => m.role)).toEqual(["system", "assistant"]);
  });
});

describe("Ledger dedupe", () => {
  it("drops the owner line and reply lines a live app turn already streamed", () => {
    const { thread, sink: s } = sink();
    const ledger = new Ledger(s);
    const turn = ledger.beginTurn("what's on today?");
    turn.bytes(chunk("Two meetings."));
    turn.bytes(chunk("\n\nLunch at 1."));
    turn.end();
    ledger.ingest([
      line("owner", "what's on today?"),
      line("eve", "Two meetings."),
      line("eve", "Lunch at 1."),
    ]);
    expect(thread).toEqual([]);
  });

  it("holds lines until the run settles, then appends only what the stream lacked", () => {
    const { thread, state, sink: s } = sink();
    const ledger = new Ledger(s);
    state.idle = false; // the page's run is in flight
    const turn = ledger.beginTurn("hi");
    ledger.ingest([line("owner", "hi"), line("eve", "hey there"), line("eve", "Reminder: call mum", { via: "other" })]);
    expect(thread).toEqual([]); // held: the reply is still arriving
    turn.bytes(chunk("hey there"));
    turn.end();
    ledger.flushIfIdle();
    expect(thread).toEqual([]); // the stream ended, but the reply is not in the store yet
    ledger.ingest([line("eve", "Later line", { via: "other" })]);
    expect(thread).toEqual([]); // still held, and kept behind the earlier held line
    thread.push({ id: "reply", role: "assistant", content: "hey there" }); // OpenUI added the reply
    state.idle = true;
    ledger.flushIfIdle();
    expect(thread.map((m) => m.role)).toEqual(["assistant", "system", "assistant"]); // after it, separated
    expect(visibleText(thread)).toEqual(["assistant:hey there", "assistant:Reminder: call mum\n\nLater line"]);
  });

  it("appends lines eve sends on its own and Telegram turns", () => {
    const { thread, sink: s } = sink();
    const ledger = new Ledger(s);
    ledger.ingest([
      line("owner", "from telegram", { via: "telegram" }),
      line("eve", "answer", { via: "telegram" }),
    ]);
    expect(visibleText(thread)).toEqual(["user:from telegram", "assistant:answer"]);
  });

  it("never shows the same transcript id twice", () => {
    const { thread, sink: s } = sink();
    const ledger = new Ledger(s);
    const a = line("eve", "once");
    ledger.markSeen([a]);
    ledger.ingest([a]);
    const b = line("eve", "twice?");
    ledger.ingest([b]);
    ledger.ingest([b]);
    expect(visibleText(thread)).toEqual(["assistant:twice?"]);
  });

  it("matches reply lines in order, so a later repeat of the same words still shows", () => {
    const { thread, sink: s } = sink();
    const ledger = new Ledger(s);
    const turn = ledger.beginTurn("ping");
    turn.bytes(chunk("ok"));
    turn.end();
    ledger.ingest([line("owner", "ping"), line("eve", "ok")]);
    ledger.ingest([line("eve", "ok")]); // eve said "ok" again, later
    expect(visibleText(thread)).toEqual(["assistant:ok"]);
  });

  it("claims an owner line once: the same words typed on Telegram later still show", () => {
    const { thread, sink: s } = sink();
    const ledger = new Ledger(s);
    ledger.beginTurn("yes").end();
    ledger.ingest([line("owner", "yes")]);
    ledger.ingest([line("owner", "yes", { via: "telegram" })]);
    expect(visibleText(thread)).toEqual(["user:yes"]);
  });

  it("separates a new eve bubble from a reply already on screen", () => {
    const { thread, sink: s } = sink();
    thread.push({ id: "r", role: "assistant", content: "earlier reply" });
    const ledger = new Ledger(s);
    ledger.ingest([line("eve", "new reminder")]);
    expect(thread.map((m) => m.role)).toEqual(["assistant", "system", "assistant"]);
  });
});

describe("storage + long poll", () => {
  function nativeStub() {
    const posts: OutboundMessage[] = [];
    const bridge = createBridge((m) => posts.push(m));
    const answer = (index: number, payload: object) => {
      const p = posts[index];
      if (p?.type !== "messages") throw new Error(`post ${index} is not a messages request`);
      bridge.native.onMessages(p.id, JSON.stringify(payload));
      return p;
    };
    return { posts, bridge, answer };
  }

  it("one thread; getMessages loads history, marks it seen and sets the cursor", async () => {
    const { posts, bridge, answer } = nativeStub();
    const { thread, sink: s } = sink();
    const ledger = new Ledger(s);
    const cursor = { value: undefined as string | undefined };
    const storage = createEveStorage(bridge, ledger, cursor, () => {});

    expect((await storage.thread.listThreads()).threads.map((t) => t.id)).toEqual(["eve"]);
    const pending = storage.thread.getMessages("eve");
    const h = line("eve", "old");
    answer(0, { messages: [line("owner", "q"), h], cursor: "c1", busy: false });
    const history = await pending;
    expect(history.map((m) => m.role)).toEqual(["user", "assistant"]);
    expect(posts[0]).not.toHaveProperty("after");
    expect(cursor.value).toBe("c1");

    ledger.ingest([h]); // the poll sees the last history line again
    expect(thread).toEqual([]);
  });

  it("long-polls with after=cursor, wait=25 idle and 2 while busy, and advances the cursor", async () => {
    const { posts, bridge, answer } = nativeStub();
    const { thread, sink: s } = sink();
    const ledger = new Ledger(s);
    const cursor = { value: "c1" as string | undefined };
    const busy: boolean[] = [];
    const tick = () => new Promise<void>((r) => setTimeout(r, 0));
    const poll = startLivePoll({ bridge, ledger, cursor, onBusy: (b) => busy.push(b), isVisible: () => true });
    await tick();
    expect(posts[0]).toMatchObject({ type: "messages", after: "c1", wait: 25 });
    answer(0, { messages: [line("eve", "ding")], cursor: "c2", busy: true });
    await tick();
    expect(visibleText(thread)).toEqual(["assistant:ding"]);
    expect(busy).toEqual([true]);
    expect(posts[1]).toMatchObject({ type: "messages", after: "c2", wait: 2 }); // busy: the end shows soon
    answer(1, { messages: [], cursor: "c2", busy: false });
    await tick();
    expect(busy).toEqual([true, false]);
    expect(posts[2]).toMatchObject({ type: "messages", after: "c2", wait: 25 });
    poll.stop();
    answer(2, { messages: [], cursor: "c2", busy: false });
    await tick();
    expect(posts).toHaveLength(3);
  });

  it("getMessages retries quietly instead of failing to the store", async () => {
    const { posts, bridge, answer } = nativeStub();
    const ledger = new Ledger(sink().sink);
    const cursor = { value: undefined as string | undefined };
    const slept: number[] = [];
    const storage = createEveStorage(bridge, ledger, cursor, () => {}, async (ms) => {
      slept.push(ms);
    });
    const pending = storage.thread.getMessages("eve");
    const first = posts[0];
    if (first?.type !== "messages") throw new Error("no request");
    bridge.native.onError(first.id, "Eve's Mac is asleep");
    await new Promise((r) => setTimeout(r, 0));
    expect(slept).toEqual([1000]);
    answer(1, { messages: [line("eve", "back")], cursor: "c9", busy: false });
    expect((await pending).map((m) => m.content)).toEqual(["back"]);
    expect(cursor.value).toBe("c9");
  });

  it("does not poll while hidden", async () => {
    const { posts, bridge } = nativeStub();
    const ledger = new Ledger(sink().sink);
    startLivePoll({ bridge, ledger, cursor: { value: "c" }, onBusy: () => {}, isVisible: () => false });
    await new Promise((r) => setTimeout(r, 0));
    expect(posts).toHaveLength(0);
  });
});
