// Sending while eve replies (sendQueue.ts, QueuedMessages.tsx; docs/app-chat.md, "Sending while eve replies").
// The page below plays NativeGlue's part (App.tsx): a send is pushed and drained at once; a started send sets
// inFlight, clears the last error and appends the owner's message, as processMessage does; a run that ends (done,
// an error, or the native Stop, which ends the stream as done) clears inFlight and drains again, and so does the
// page showing again.

import type { Message } from "@openuidev/react-headless";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Attachment } from "../attachments";
import type { WireMessage } from "../nativeBridge";
import { QueuedList } from "../QueuedMessages";
import { mayStart, SendQueue, type QueuedSend, type StartGate } from "../sendQueue";
import { Ledger, type LedgerSink } from "../sync";

const PHOTO: Attachment = { id: "a".repeat(32), name: "Receipt.jpg", kind: "image" };

function page({ historyLoaded = true } = {}) {
  const queue = new SendQueue();
  const thread: string[] = [];          // the owner's messages OpenUI shows, in order
  const state = { inFlight: false, running: false, loading: false, historyLoaded, failed: false, hidden: false };
  const gate = (): StartGate => ({
    inFlight: state.inFlight,
    running: state.running,
    loading: state.loading,
    historyLoaded: state.historyLoaded,
    hiddenAfterError: state.failed && state.hidden,
  });
  const drain = () => {
    const next = queue.take(gate());
    if (next === undefined) return;
    state.inFlight = true;
    state.running = true;
    state.failed = false;
    thread.push(next.text);
  };
  return {
    queue,
    thread,
    state,
    send(text: string, attachments: Attachment[] = []) {
      queue.push(text, attachments);
      drain();
    },
    /** The run on the screen ends: finished, stopped from the composer (both "done"), or failed. */
    settle(how: "done" | "error" = "done") {
      state.inFlight = false;
      state.running = false;
      state.failed = how === "error";
      drain();
    },
    loadHistory() {
      state.historyLoaded = true;
      drain();
    },
    hide() {
      state.hidden = true;
    },
    show() {
      state.hidden = false;
      drain();
    },
    /** Everything the owner sees of their own words: the thread, then the queued bubbles. */
    screen: () => [...thread, ...queue.snapshot().map((q) => `${q.text} (queued)`)],
  };
}

const texts = (items: readonly QueuedSend[]) => items.map((q) => q.text);
const IDLE: StartGate = { inFlight: false, running: false, loading: false, historyLoaded: true, hiddenAfterError: false };

describe("mayStart", () => {
  it("starts only when nothing of the page's runs and the thread has loaded", () => {
    const idle = IDLE;
    expect(mayStart(idle)).toBe(true);
    expect(mayStart({ ...idle, inFlight: true })).toBe(false);
    expect(mayStart({ ...idle, running: true })).toBe(false);
    expect(mayStart({ ...idle, loading: true })).toBe(false);
    expect(mayStart({ ...idle, historyLoaded: false })).toBe(false);
    expect(mayStart({ ...idle, hiddenAfterError: true })).toBe(false);
  });
});

describe("SendQueue", () => {
  it("an idle send starts at once and is never shown as queued", () => {
    const p = page();
    p.send("hi");
    expect(p.thread).toEqual(["hi"]);
    expect(p.queue.snapshot()).toEqual([]);
    expect(p.screen()).toEqual(["hi"]);
  });

  it("sends made while eve replies wait in the order they were sent", () => {
    const p = page();
    p.send("first");
    p.send("second");
    p.send("third");
    p.send("fourth");
    expect(p.thread).toEqual(["first"]);
    expect(texts(p.queue.snapshot())).toEqual(["second", "third", "fourth"]);
    expect(p.screen()).toEqual(["first", "second (queued)", "third (queued)", "fourth (queued)"]);
    p.settle();
    expect(p.thread).toEqual(["first", "second"]);
    p.settle();
    p.settle();
    expect(p.thread).toEqual(["first", "second", "third", "fourth"]);
    expect(p.queue.size).toBe(0);
  });

  it("a queued send leaves the queue in the same step it joins the thread: never shown twice, never lost", () => {
    const p = page();
    const words = ["one", "two", "three"];
    const times = (shown: string[], word: string) => shown.filter((s) => s === word || s === `${word} (queued)`).length;
    // While a step runs (what a listener sees mid-call), a send is never in both places.
    p.queue.subscribe(() => {
      for (const word of words) expect(times(p.screen(), word)).toBeLessThanOrEqual(1);
    });
    // React renders once the step is over: by then every send shows exactly once.
    const steps = [() => p.send("one"), () => p.send("two"), () => p.send("three"), p.settle, p.settle, p.settle];
    for (const step of steps) {
      step();
      for (const word of words.slice(0, Math.min(3, p.thread.length + p.queue.size))) {
        expect(times(p.screen(), word), `${word} in ${JSON.stringify(p.screen())}`).toBe(1);
      }
    }
    expect(p.screen()).toEqual(["one", "two", "three"]);
  });

  it("Stop ends the reply and the next queued send goes, then the one after it", () => {
    const p = page();
    p.send("long question");
    p.send("actually, shorter");
    p.send("and one more");
    p.settle();                                         // the owner tapped Stop on the first reply
    expect(p.thread).toEqual(["long question", "actually, shorter"]);
    expect(texts(p.queue.snapshot())).toEqual(["and one more"]);
    expect(p.state.inFlight).toBe(true);                // the next one is running now
    p.settle();                                         // Stop again
    expect(p.thread.at(-1)).toBe("and one more");
    expect(p.queue.snapshot()).toEqual([]);
    p.settle();
    expect(p.thread).toHaveLength(3);
  });

  it("a reply that fails goes on to the next send while the page shows", () => {
    const p = page();
    p.send("one");
    p.send("two");
    p.settle("error");
    expect(p.thread).toEqual(["one", "two"]);
  });

  it("after a stream cut in the background, the next send waits until the page shows again", () => {
    const p = page();
    p.send("before bed");
    p.send("and this");
    p.send("and that");
    p.hide();
    p.settle("done");                                   // finished in the background grace: the next one goes
    expect(p.thread).toEqual(["before bed", "and this"]);
    p.settle("error");                                  // the app cut this stream after 25 s hidden
    expect(p.thread).toEqual(["before bed", "and this"]);
    expect(texts(p.queue.snapshot())).toEqual(["and that"]);
    p.show();
    expect(p.thread).toEqual(["before bed", "and this", "and that"]);
  });

  it("one more send while the queue drains goes to the end of the line", () => {
    const p = page();
    p.send("a");
    p.send("b");
    p.settle();
    p.send("c");
    expect(texts(p.queue.snapshot())).toEqual(["c"]);
    p.send("d");
    p.settle();
    p.settle();
    expect(p.thread).toEqual(["a", "b", "c", "d"]);
  });

  it("waits for the thread's history, then starts in order", () => {
    const p = page({ historyLoaded: false });
    p.send("early");
    p.send("earlier still waiting");
    expect(p.thread).toEqual([]);
    expect(texts(p.queue.snapshot())).toEqual(["early", "earlier still waiting"]);
    p.loadHistory();
    expect(p.thread).toEqual(["early"]);
    p.settle();
    expect(p.thread).toEqual(["early", "earlier still waiting"]);
  });

  it("keeps each send's attachments and gives every send its own key", () => {
    const p = page();
    p.send("busy");
    p.send("", [PHOTO]);
    p.send("same words");
    p.send("same words");
    const [photo, x, y] = p.queue.snapshot();
    expect(photo!.attachments).toEqual([PHOTO]);
    expect(x!.key).not.toBe(y!.key);
    expect(new Set(p.queue.snapshot().map((q) => q.key)).size).toBe(3);
  });

  it("hands React the same snapshot until the queue changes, and a new frozen one after", () => {
    const q = new SendQueue();
    const empty = q.snapshot();
    expect(q.snapshot()).toBe(empty);
    expect(q.take({ ...IDLE })).toBeUndefined();
    expect(q.snapshot()).toBe(empty);                   // nothing taken: nothing changed
    q.push("x");
    const one = q.snapshot();
    expect(one).not.toBe(empty);
    expect(q.snapshot()).toBe(one);
    expect(Object.isFrozen(one)).toBe(true);
    expect(q.take({ ...IDLE, inFlight: true, running: true })).toBeUndefined();
    expect(q.snapshot()).toBe(one);                     // a refused take changes nothing
  });

  it("tells listeners on every change and stops when they leave", () => {
    const q = new SendQueue();
    let heard = 0;
    const off = q.subscribe(() => heard++);
    q.push("a");
    q.take({ ...IDLE });
    expect(heard).toBe(2);
    off();
    q.push("b");
    expect(heard).toBe(2);
  });
});

describe("Stop with a message queued (sync.ts)", () => {
  let n = 0;
  const line = (role: WireMessage["role"], text: string): WireMessage => ({
    id: `q${++n}`, role, text, t: 5000 + n * 1000, via: "app", kind: "say",
  });
  const chunk = (content: string) => `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content } }] })}\n\n`;

  it("the rest of a stopped reply, which eve finishes on the Mac first, comes after the queued turn's reply, never inside it", () => {
    const thread: Message[] = [{ id: "askA", role: "user", content: "plan my week" }];
    const state = { idle: false };
    const sink: LedgerSink = {
      append: (...m) => {
        thread.push(...m);
      },
      lastRole: () => thread.at(-1)?.role,
      idle: () => state.idle,
    };
    const ledger = new Ledger(sink);
    const a = ledger.beginTurn("plan my week", "askA");
    a.bytes(chunk("Monday: gym."));
    // Stop: the page ends A as done; the queued message starts at once
    a.end();
    thread.push({ id: "replyA", role: "assistant", content: "Monday: gym." });
    state.idle = true;
    ledger.flushIfIdle();
    thread.push({ id: "askB", role: "user", content: "thanks" });
    state.idle = false;
    const b = ledger.beginTurn("thanks", "askB");
    // the daemon takes one turn at a time: it finishes A while B's stream waits, and the poll brings A's lines
    ledger.ingest([line("owner", "plan my week"), line("eve", "Monday: gym."), line("eve", "Tuesday: dentist.")]);
    expect(thread.map((m) => m.id)).toEqual(["askA", "replyA", "askB"]);   // held while B streams
    b.bytes(chunk("You're welcome."));
    b.end();
    thread.push({ id: "replyB", role: "assistant", content: "You're welcome." });
    state.idle = true;
    ledger.flushIfIdle();
    ledger.ingest([line("owner", "thanks"), line("eve", "You're welcome.")]);  // B's own copy: dropped
    const shown = thread.filter((m) => m.role !== "system").map((m) => String(m.content));
    expect(shown).toEqual(["plan my week", "Monday: gym.", "thanks", "You're welcome.", "Tuesday: dentist."]);
  });
});

describe("QueuedList", () => {
  const item = (key: string, text: string, attachments: Attachment[] = []): QueuedSend => ({ key, text, attachments });

  it("draws nothing when nothing waits", () => {
    expect(renderToStaticMarkup(createElement(QueuedList, { items: [] }))).toBe("");
  });

  it("draws each queued send as the owner's bubble, in order, each marked Queued", () => {
    const html = renderToStaticMarkup(
      createElement(QueuedList, { items: [item("queued:1", "first <b>words</b>"), item("queued:2", "second")] }),
    );
    expect(html).toContain('class="eve-queued"');
    expect(html).toContain('aria-label="Queued messages"');
    expect(html.match(/class="eve-queued__item"/g)).toHaveLength(2);
    expect(html.match(/>Queued</g)).toHaveLength(2);
    expect(html.match(/openui-shell-thread-message-user__content/g)).toHaveLength(2);
    expect(html.indexOf("first")).toBeLessThan(html.indexOf("second"));
    expect(html).toContain("first &lt;b&gt;words&lt;/b&gt;");         // the owner's words are text, never markup
    expect(html).not.toContain("data-mid");                           // not a message Horizontal History can jump to
  });

  it("shows a queued send's photo above its bubble, as the sent message will", () => {
    const html = renderToStaticMarkup(createElement(QueuedList, { items: [item("queued:1", "look", [PHOTO])] }));
    expect(html).toContain("eve-att-message");
    expect(html).toContain("Receipt.jpg");
    expect(html).toContain("look");
  });
});
