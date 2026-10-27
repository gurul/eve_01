// Sources under a reply (docs/app-chat.md, "Sources"): an eve row's
// "sources" are sanitized, kept beside the message by id, carried onto the
// bubble a merged line joins, handed to the page's own streamed reply when
// the poll's copy of it is claimed, and drawn as "Sources · N".

import type { Message } from "@openuidev/react-headless";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { WireMessage } from "../nativeBridge";
import { SourceList } from "../SourceList";
import {
  MAX_SOURCE_CHARS,
  MAX_SOURCES,
  MAX_WHEN_CHARS,
  SourceStore,
  sanitizeSources,
  sources as pageSources,
  type Source,
} from "../sources";
import { Ledger, mapLines, replyAfter, type LedgerSink } from "../sync";

let n = 0;
const line = (role: WireMessage["role"], text: string, extra: Partial<WireMessage> = {}): WireMessage => ({
  id: `s${++n}`,
  role,
  text,
  t: 9000 + n * 1000,
  via: "app",
  kind: "say",
  ...extra,
});

const FACT: Source = { kind: "fact", text: "plumber: comes on Fridays", when: "2026-09-30" };
const SAID: Source = { kind: "said", text: "You: the plumber is booked for Friday", when: "2026-10-02 18:40" };

const chunk = (content: string) =>
  `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content } }] })}\n\n`;

describe("sanitizeSources", () => {
  it("keeps well-formed entries only, with string fields", () => {
    expect(
      sanitizeSources([
        FACT,
        { kind: "script", text: "<b>x</b>" },
        { kind: "said", text: 4 },
        "nope",
        null,
        { kind: "remembered", text: "  ", when: "x" },
        { kind: "remembered", text: "prefers mornings", when: 7 },
      ]),
    ).toEqual([FACT, { kind: "remembered", text: "prefers mornings" }]);
    expect(sanitizeSources("x")).toEqual([]);
    expect(sanitizeSources(undefined)).toEqual([]);
    expect(sanitizeSources({ 0: FACT })).toEqual([]);
  });

  it("caps the count and the lengths, and drops repeats", () => {
    const many = Array.from({ length: 10 }, (_, i) => ({ kind: "fact", text: `f${i} ${"x".repeat(400)}`, when: "w".repeat(90) }));
    const got = sanitizeSources([FACT, FACT, ...many]);
    expect(got).toHaveLength(MAX_SOURCES);
    expect(got.filter((s) => s.text === FACT.text)).toHaveLength(1);
    expect(got.every((s) => s.text.length <= MAX_SOURCE_CHARS && (s.when ?? "").length <= MAX_WHEN_CHARS)).toBe(true);
  });
});

describe("mapping history", () => {
  it("keeps an eve row's sources beside its message, and none on an owner row", () => {
    const store = new SourceStore();
    const ask = line("owner", "when?", { sources: [FACT] });
    const reply = line("eve", "Friday.", { sources: [FACT, { kind: "bad", text: "x" }] });
    const plain = line("eve", "Anything else?", { t: reply.t + 600 });
    const msgs = mapLines([ask, reply, plain], undefined, store);
    expect(msgs.map((m) => m.role)).toEqual(["user", "assistant", "system", "assistant"]);
    expect(store.sourcesOf(ask.id)).toEqual([]);
    expect(store.sourcesOf(reply.id)).toEqual([FACT]);
    expect(store.sourcesOf(plain.id)).toEqual([]);
  });

  it("a line merged into the bubble before it adds its sources to that bubble", () => {
    const store = new SourceStore();
    const first = line("eve", "Friday.", { sources: [FACT] });
    const second = line("eve", "In the morning.", { t: first.t + 30, sources: [FACT, SAID] });
    const msgs = mapLines([first, second], undefined, store);
    expect(msgs).toHaveLength(1);
    expect(store.sourcesOf(first.id)).toEqual([FACT, SAID]);
  });

  it("a reload replaces what a message had, and an empty row clears it", () => {
    const store = new SourceStore();
    const reply = line("eve", "Friday.", { sources: [FACT] });
    mapLines([reply], undefined, store);
    mapLines([{ ...reply, sources: [SAID] }], undefined, store);
    expect(store.sourcesOf(reply.id)).toEqual([SAID]);
    mapLines([{ ...reply, sources: undefined }], undefined, store);
    expect(store.sourcesOf(reply.id)).toEqual([]);
  });

  it("notifies only when a message's sources change", () => {
    const store = new SourceStore();
    let heard = 0;
    store.subscribe(() => heard++);
    store.set("a", [FACT]);
    store.set("a", [FACT]);
    store.add("a", [FACT]);
    store.set("b", []);
    expect(heard).toBe(1);
  });
});

describe("a live turn", () => {
  it("hands the claimed copy's sources to the page's own streamed reply", () => {
    const thread: Message[] = [{ id: "ask", role: "user", content: "when does the plumber come?" }];
    const handed: Source[][] = [];
    const sink: LedgerSink = {
      append: (...m) => {
        thread.push(...m);
      },
      lastRole: () => thread.at(-1)?.role,
      attachSources: (s) => handed.push(s),
    };
    const ledger = new Ledger(sink);
    const turn = ledger.beginTurn("when does the plumber come?");
    turn.bytes(chunk("On Friday."));
    turn.end();
    thread.push({ id: "reply", role: "assistant", content: "On Friday." });
    ledger.ingest([line("owner", "when does the plumber come?"), line("eve", "On Friday.", { sources: [FACT] })]);
    expect(thread.map((m) => m.id)).toEqual(["ask", "reply"]); // the copy is dropped
    expect(handed).toEqual([[FACT]]);
  });

  it("back to back (a queued message started as the reply ended): a late copy's sources go on its own turn's reply", () => {
    const thread: Message[] = [{ id: "askA", role: "user", content: "when does the plumber come?" }];
    const store = new SourceStore();
    // as App.tsx's sink
    const sink: LedgerSink = {
      append: (...m) => {
        thread.push(...m);
      },
      lastRole: () => thread.at(-1)?.role,
      attachSources: (s, userId) => {
        const reply = replyAfter(thread, userId);
        if (reply) store.add(reply.id, s);
      },
    };
    const ledger = new Ledger(sink);
    const a = ledger.beginTurn("when does the plumber come?", "askA");
    a.bytes(chunk("On Friday."));
    a.end();
    thread.push({ id: "replyA", role: "assistant", content: "On Friday." });
    // the queued message starts at once, and A's copy only arrives while B streams
    thread.push({ id: "askB", role: "user", content: "and the electrician?" });
    const b = ledger.beginTurn("and the electrician?", "askB");
    b.bytes(chunk("Not booked yet."));
    thread.push({ id: "replyB", role: "assistant", content: "Not booked yet." });
    ledger.ingest([line("owner", "when does the plumber come?"), line("eve", "On Friday.", { sources: [FACT] })]);
    b.end();
    ledger.flushIfIdle();
    expect(thread.map((m) => m.id)).toEqual(["askA", "replyA", "askB", "replyB"]); // the copy is dropped
    expect(store.sourcesOf("replyA")).toEqual([FACT]);
    expect(store.sourcesOf("replyB")).toEqual([]);
  });

  it("replyAfter finds a turn's own reply, and the last turn's without an id", () => {
    const thread: Message[] = [
      { id: "u1", role: "user", content: "a" },
      { id: "r1", role: "assistant", content: "x" },
      { id: "sep", role: "system", content: "" } as Message,
      { id: "r1b", role: "assistant", content: "y" },
      { id: "u2", role: "user", content: "b" },
      { id: "r2", role: "assistant", content: "z" },
      { id: "u3", role: "user", content: "c" },
    ];
    expect(replyAfter(thread, "u1")?.id).toBe("r1b");
    expect(replyAfter(thread, "u2")?.id).toBe("r2");
    expect(replyAfter(thread, "u3")).toBeUndefined();               // its reply is not in yet
    expect(replyAfter(thread)).toBeUndefined();                     // the last owner message has none
    expect(replyAfter(thread.slice(0, 6))?.id).toBe("r2");
    expect(replyAfter(thread.slice(0, 6), "gone")?.id).toBe("r2");  // an id not in the thread: the last turn's
    expect(replyAfter([])).toBeUndefined();
  });

  it("a claimed copy without sources hands nothing", () => {
    const handed: Source[][] = [];
    const ledger = new Ledger({ append: () => {}, lastRole: () => "assistant", attachSources: (s) => handed.push(s) });
    const turn = ledger.beginTurn("hi");
    turn.bytes(chunk("Hello."));
    turn.end();
    ledger.ingest([line("owner", "hi"), line("eve", "Hello.")]);
    expect(handed).toEqual([]);
  });
});

describe("SourceList", () => {
  it("draws nothing without sources, and a folded count with them", () => {
    expect(renderToStaticMarkup(createElement(SourceList, { messageId: "x", items: [] }))).toBe("");
    const html = renderToStaticMarkup(createElement(SourceList, { messageId: "y", items: [FACT, SAID] }));
    expect(html).toContain("Sources");
    expect(html).toContain("<span>2</span>");
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("plumber"); // folded at rest
  });

  it("open, shows each source with its kind and when, its words escaped as text", () => {
    const evil: Source = { kind: "said", text: "<img src=x onerror=alert(1)>" };
    const html = renderToStaticMarkup(
      createElement(SourceList, { messageId: "z", items: [FACT, evil], defaultOpen: true }),
    );
    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain("Fact");
    expect(html).toContain("2026-09-30");
    expect(html).toContain("plumber: comes on Fridays");
    expect(html).toContain("&lt;img src=x onerror=alert(1)&gt;");
    expect(html).not.toContain("<img");
    expect(pageSources).toBeInstanceOf(SourceStore);
  });
});
