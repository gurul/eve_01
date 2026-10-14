import { afterEach, describe, expect, it, vi } from "vitest";
import { createBridge, type EveNative, type OutboundMessage } from "../../nativeBridge";
import { createWebHost, DEVICE_UNPAIRED, TURN_IDLE_MS, type Grade } from "../host";
import { LinkLost, type LinkRequest, type Sink } from "../linkChannel";
import { Unreachable, type RequestOptions } from "../transport";

const enc = new TextEncoder();
const sse = (content: string) => `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content } }] })}\n\n`;

interface Sent {
  req: LinkRequest;
  sink: Sink;
  opts: RequestOptions;
  cancelled: boolean;
}

function setup(where: "mac" | "lan" = "lan") {
  const sent: Sent[] = [];
  const transport = {
    request(req: LinkRequest, sink: Sink, opts: RequestOptions = {}) {
      const s: Sent = { req, sink, opts, cancelled: false };
      sent.push(s);
      return {
        cancel: () => {
          s.cancelled = true;
        },
        via: Promise.resolve(opts.via === "fetch" ? ("fetch" as const) : ("channel" as const)),
      };
    },
  };
  let grade: Grade = "auto";
  const calls: string[] = [];
  const native: EveNative = {
    onChunk: vi.fn((id: string, text: string) => calls.push(`chunk:${id}:${text}`)),
    onDone: vi.fn((id: string) => calls.push(`done:${id}`)),
    onError: vi.fn((id: string, m: string) => calls.push(`error:${id}:${m}`)),
    onMessages: vi.fn((id: string, json: string) => calls.push(`messages:${id}:${json}`)),
    send: vi.fn(),
    setBusy: vi.fn(),
    scrollToBottom: vi.fn(),
    setInsets: vi.fn(),
  };
  const openWindow = vi.fn();
  const openScheme = vi.fn();
  const openAttachment = vi.fn();
  const onAuthLost = vi.fn();
  const host = createWebHost({
    where,
    transport,
    grade: () => grade,
    openWindow,
    openScheme,
    openAttachment,
    onAuthLost,
  });
  host.attach(native);
  return {
    host,
    native,
    sent,
    calls,
    openWindow,
    openScheme,
    openAttachment,
    onAuthLost,
    setGrade: (g: Grade) => {
      grade = g;
    },
  };
}

const turn = (over: Partial<Extract<OutboundMessage, { type: "turn" }>> = {}): OutboundMessage => ({
  type: "turn",
  id: "t1",
  text: "  hi eve  ",
  client_id: "c-1",
  ...over,
});

function stream(s: Sent, ...parts: string[]) {
  for (const p of parts) s.sink.data(enc.encode(p));
}

afterEach(() => {
  vi.useRealTimers();
});

describe("webPost: the bridge's messages, done in the browser", () => {
  it("a turn is POST /api/link/turn with the words, client_id and grade; its SSE goes to onChunk, then onDone", () => {
    const t = setup();
    t.host.post(turn({ attachments: ["0123456789abcdef0123456789abcdef"] }));
    expect(t.sent).toHaveLength(1);
    const s = t.sent[0]!;
    expect(s.req).toMatchObject({ method: "POST", path: "/api/link/turn", query: "" });
    expect(s.req.headers["content-type"]).toBe("application/json");
    expect(JSON.parse(s.req.body)).toEqual({
      text: "hi eve",
      client_id: "c-1",
      grade: "auto",
      attachments: ["0123456789abcdef0123456789abcdef"],
    });
    expect(t.host.getState().busy).toBe(true);
    expect(t.native.setBusy).toHaveBeenLastCalledWith(true);
    s.sink.head(200, { "content-type": "text/event-stream" });
    const wire = sse("Hello") + ": ping\n\n" + sse(" there") + "data: [DONE]\n\n";
    stream(s, wire.slice(0, 13), wire.slice(13, 70), wire.slice(70));
    s.sink.end();
    expect(t.calls.filter((c) => c.startsWith("chunk:")).map((c) => c.slice("chunk:t1:".length)).join("")).toBe(wire);
    expect(t.calls.at(-1)).toBe("done:t1");
    expect(t.host.getState().busy).toBe(false);
    expect(t.native.setBusy).toHaveBeenLastCalledWith(false);
  });

  it("the Think toggle sends grade think", () => {
    const t = setup();
    t.setGrade("think");
    t.host.post(turn());
    expect(JSON.parse(t.sent[0]!.req.body)).toMatchObject({ grade: "think" });
  });

  it("a refused turn ends with the daemon's own line; a 401 also reports the lost pairing", () => {
    const t = setup();
    t.host.post(turn());
    const s = t.sent[0]!;
    s.sink.head(503, { "content-type": "application/json" });
    stream(s, JSON.stringify({ ok: false, error: "eve's chat isn't running, or no owner has texted it yet" }));
    s.sink.end();
    expect(t.calls.at(-1)).toBe("error:t1:eve's chat isn't running, or no owner has texted it yet");
    expect(t.host.getState().problem).toBe("eve's chat isn't running, or no owner has texted it yet");

    t.host.post(turn({ id: "t2", client_id: "c-2" }));
    const s2 = t.sent[1]!;
    s2.sink.head(401, {});
    stream(s2, '{"ok":false,"error":"the link token is missing or wrong"}');
    s2.sink.end();
    expect(t.onAuthLost).toHaveBeenCalledTimes(1);
    expect(t.calls.at(-1)).toBe(`error:t2:${DEVICE_UNPAIRED}`);
  });

  it("messages: GET /api/link/messages with after and wait; the JSON goes to onMessages", () => {
    const t = setup();
    t.host.post({ type: "messages", id: "m1", after: "c9~2", wait: 25 });
    const s = t.sent[0]!;
    expect(s.req).toMatchObject({ method: "GET", path: "/api/link/messages", query: "after=c9%7E2&wait=25", body: "" });
    s.sink.head(200, {});
    const page = '{"messages":[],"cursor":"c10","busy":false}';
    stream(s, page.slice(0, 10), page.slice(10));
    s.sink.end();
    expect(t.calls).toEqual([`messages:m1:${page}`]);
  });

  it("the first page says whether the conversation is empty; a turn ends that", () => {
    const t = setup();
    t.host.post({ type: "messages", id: "h", wait: 0 });
    expect(t.sent[0]!.req.query).toBe("");
    t.sent[0]!.sink.head(200, {});
    stream(t.sent[0]!, '{"messages":[],"cursor":"c1","busy":false}');
    t.sent[0]!.sink.end();
    expect(t.host.getState().historyEmpty).toBe(true);
    t.host.post(turn());
    expect(t.host.getState().historyEmpty).toBe(false);
  });

  it("cancel stops the request and says nothing more to the page", () => {
    const t = setup();
    t.host.post(turn());
    t.sent[0]!.sink.head(200, {});
    t.host.post({ type: "cancel", id: "t1" });
    expect(t.sent[0]!.cancelled).toBe(true);
    stream(t.sent[0]!, sse("late"));
    t.sent[0]!.sink.end();
    expect(t.calls).toEqual([]);
    expect(t.host.getState().busy).toBe(false);
  });

  it("Stop ends this page's turns as finished replies (onDone), as the iOS app's stop button does", () => {
    const t = setup();
    t.host.post(turn());
    t.host.post({ type: "messages", id: "m1", after: "c", wait: 25 });
    t.sent[0]!.sink.head(200, {});
    stream(t.sent[0]!, sse("half"));
    t.host.stop();
    expect(t.sent[0]!.cancelled).toBe(true);
    expect(t.sent[1]!.cancelled).toBe(false);
    expect(t.calls.at(-1)).toBe("done:t1");
    expect(t.host.getState().busy).toBe(false);
  });

  it("a direct link that drops mid-reply carries on over HTTPS: same turn, nothing shown twice", () => {
    const t = setup();
    t.host.post(turn());
    const first = t.sent[0]!;
    first.sink.head(200, {});
    stream(first, sse("One."), sse(" Two."), "data: {\"choi"); // the third event is cut mid-way
    first.sink.fail(new LinkLost());
    expect(t.sent).toHaveLength(2);
    const retry = t.sent[1]!;
    expect(retry.opts).toEqual({ via: "fetch" });
    expect(retry.req.body).toBe(first.req.body);
    retry.sink.head(200, {});
    // the daemon streams the same turn from its first chunk again, pings at other places
    stream(retry, ": ping\n\n", sse("One."), sse(" Two."), sse(" Three."), "data: [DONE]\n\n");
    retry.sink.end();
    const shown = t.calls.filter((c) => c.startsWith("chunk:")).map((c) => c.slice("chunk:t1:".length)).join("");
    expect(shown).toBe(sse("One.") + sse(" Two.") + sse(" Three.") + "data: [DONE]\n\n");
    expect(t.calls.at(-1)).toBe("done:t1");
  });

  it("a turn older than 5 minutes is not asked again (the daemon would hear it twice)", () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const t = setup();
    t.host.post(turn());
    t.sent[0]!.sink.head(200, {});
    for (let i = 0; i < 25; i++) {
      vi.advanceTimersByTime(14_000);
      stream(t.sent[0]!, ": ping\n\n"); // a long turn, kept alive by the daemon's pings
    }
    t.sent[0]!.sink.fail(new LinkLost());
    expect(t.sent).toHaveLength(1);
    expect(t.calls.at(-1)).toMatch(/^error:t1:The direct link to eve dropped/);
  });

  it("a second drop ends the turn with a line, rather than retrying for ever", () => {
    const t = setup();
    t.host.post(turn());
    t.sent[0]!.sink.fail(new LinkLost());
    t.sent[1]!.sink.fail(new LinkLost());
    expect(t.sent).toHaveLength(2);
    expect(t.calls.at(-1)).toMatch(/^error:t1:The direct link to eve dropped/);
  });

  it("no answer at all over HTTPS is one plain line; the Mac's own page says eve may be restarting", () => {
    const lan = setup("lan");
    lan.host.post(turn());
    lan.sent[0]!.sink.fail(new Unreachable());
    expect(lan.calls.at(-1)).toMatch(/^error:t1:Can't reach eve/);
    const mac = setup("mac");
    mac.host.post(turn());
    mac.sent[0]!.sink.fail(new Unreachable());
    expect(mac.calls.at(-1)).toMatch(/^error:t1:eve isn't answering on this Mac/);
  });

  it("a stream silent for 45 s is retried once over HTTPS, then given up with a line", () => {
    vi.useFakeTimers();
    const t = setup();
    t.host.post(turn());
    t.sent[0]!.sink.head(200, {});
    vi.advanceTimersByTime(TURN_IDLE_MS);
    expect(t.sent[0]!.cancelled).toBe(true);
    expect(t.sent).toHaveLength(2);
    expect(t.sent[1]!.opts).toEqual({ via: "fetch" });
    vi.advanceTimersByTime(TURN_IDLE_MS);
    expect(t.sent[1]!.cancelled).toBe(true);
    expect(t.calls.at(-1)).toMatch(/^error:t1:eve stopped answering/);
  });

  it("a turn native would refuse never leaves the page", () => {
    const t = setup();
    t.host.post(turn({ text: "x".repeat(4001) }));
    t.host.post(turn({ id: "t2", text: "   " }));
    t.host.post(turn({ id: "t3", attachments: ["../../etc/passwd"] }));
    t.host.post(turn({ id: "t4", client_id: "" }));
    expect(t.sent).toHaveLength(0);
    expect(t.calls.map((c) => c.split(":").slice(0, 2).join(":"))).toEqual(["error:t1", "error:t2", "error:t3", "error:t4"]);
    // 4000 emoji are 4000 characters as the daemon counts them, though 8000 UTF-16 units
    t.host.post(turn({ id: "t5", text: "😀".repeat(4000) }));
    expect(t.sent).toHaveLength(1);
  });

  it("openURL: a web page in a new tab, mail and calls to the system, anything else nowhere", () => {
    const t = setup();
    t.host.post({ type: "openURL", url: "https://example.com/a?b=1" });
    t.host.post({ type: "openURL", url: "mailto:someone@example.com" });
    t.host.post({ type: "openURL", url: "tel:+15555550123" });
    t.host.post({ type: "openURL", url: "javascript:alert(1)" });
    t.host.post({ type: "openURL", url: "file:///etc/passwd" });
    t.host.post({ type: "openURL", url: "not a url" });
    expect(t.openWindow.mock.calls).toEqual([["https://example.com/a?b=1"]]);
    expect(t.openScheme.mock.calls).toEqual([["mailto:someone@example.com"], ["tel:+15555550123"]]);
  });

  it("openAttachment takes a well-formed upload id only", () => {
    const t = setup();
    t.host.post({ type: "openAttachment", id: "0123456789abcdef0123456789abcdef", name: "Photo", kind: "image" });
    t.host.post({ type: "openAttachment", id: "../x", name: "x", kind: "image" });
    expect(t.openAttachment.mock.calls).toEqual([[{ id: "0123456789abcdef0123456789abcdef", name: "Photo", kind: "image" }]]);
  });

  it("ready: the composer may send, and the shell's insets reach the page", () => {
    const t = setup();
    t.host.setInsets({ top: 70, bottom: 120 });
    expect(t.native.setInsets).toHaveBeenLastCalledWith({ top: 70, bottom: 120 });
    expect(t.host.getState().ready).toBe(false);
    t.host.post({ type: "ready" });
    expect(t.host.getState().ready).toBe(true);
    expect(t.native.setInsets).toHaveBeenLastCalledWith({ top: 70, bottom: 120 });
  });

  it("drives the page's real bridge: a turn opened by the chat streams back through it", async () => {
    const sent: Sent[] = [];
    const host = createWebHost({
      where: "lan",
      transport: {
        request(req, sink, opts = {}) {
          sent.push({ req, sink, opts, cancelled: false });
          return { cancel() {}, via: Promise.resolve("channel" as const) };
        },
      },
      grade: () => "auto",
      openWindow() {},
      openScheme() {},
      openAttachment() {},
    });
    const bridge = createBridge(host.post);
    host.attach(bridge.native);
    const response = bridge.openTurn({ text: "hello", clientId: "c-9", signal: new AbortController().signal });
    const s = sent[0]!;
    s.sink.head(200, {});
    stream(s, sse("Hi!"), "data: [DONE]\n\n");
    s.sink.end();
    expect(await response.text()).toBe(sse("Hi!") + "data: [DONE]\n\n");
  });
});
