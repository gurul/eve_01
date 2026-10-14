import { afterEach, describe, expect, it, vi } from "vitest";
import { encodeData, encodeEnd, encodeErr, encodeHead, MAX_IN_FLIGHT } from "../framing";
import { LinkChannel, LinkLost, PROBE_PATH, type LinkRequest } from "../linkChannel";
import { FakePort, recorder } from "./fakes";

const messages = (query = "after=c1&wait=25"): LinkRequest => ({
  method: "GET",
  path: "/api/link/messages",
  query,
  headers: { accept: "application/json" },
  body: "",
});

afterEach(() => {
  vi.useRealTimers();
});

describe("LinkChannel", () => {
  it("sends a request as one req frame with lowercase headers and no origin of its own", () => {
    const port = new FakePort();
    const ch = new LinkChannel(port);
    ch.request(
      { method: "POST", path: "/api/link/turn", query: "", headers: { "Content-Type": "application/json" }, body: "{}" },
      recorder().sink,
    );
    expect(port.frames()).toEqual([
      { t: "req", id: 1, m: "POST", p: "/api/link/turn", q: "", h: { "content-type": "application/json" }, b: "{}" },
    ]);
  });

  it("reassembly of split frames: a 100 KB messages page comes back whole, in order", () => {
    const port = new FakePort();
    const ch = new LinkChannel(port);
    const r = recorder();
    ch.request(messages(), r.sink);
    const id = port.lastReqId();
    const page = JSON.stringify({
      messages: Array.from({ length: 900 }, (_, i) => ({ id: `l${i}`, role: "eve", text: `line ${i} ${"é".repeat(40)}`, t: i, via: "app", kind: "say" })),
      cursor: "c2",
      busy: false,
    });
    const bytes = new TextEncoder().encode(page);
    expect(bytes.length).toBeGreaterThan(100_000);
    port.deliver(encodeHead(id, 200, { "content-type": "application/json" }));
    const frames = encodeData(id, bytes);
    expect(frames.length).toBeGreaterThan(6);
    // the frames as the browser hands them over: each its own ArrayBuffer
    for (const f of frames) port.deliver(f.slice().buffer);
    expect(r.got.ended).toBe(false);
    port.deliver(encodeEnd(id));
    expect(r.got.ended).toBe(true);
    expect(r.got.status).toBe(200);
    expect(r.text()).toBe(page);
  });

  it("keeps two answers apart by id", () => {
    const port = new FakePort();
    const ch = new LinkChannel(port);
    const a = recorder();
    const b = recorder();
    ch.request(messages("a=1"), a.sink);
    const idA = port.lastReqId();
    ch.request(messages("b=1"), b.sink);
    const idB = port.lastReqId();
    port.deliver(encodeHead(idB, 200));
    port.deliver(encodeHead(idA, 503));
    for (const f of encodeData(idB, new TextEncoder().encode("B"))) port.deliver(f);
    for (const f of encodeData(idA, new TextEncoder().encode("A"))) port.deliver(f);
    port.deliver(encodeEnd(idA));
    port.deliver(encodeEnd(idB));
    expect([a.got.status, a.text()]).toEqual([503, "A"]);
    expect([b.got.status, b.text()]).toEqual([200, "B"]);
  });

  it("takes at most 8 requests at once; the next one fails at once with LinkLost", async () => {
    const port = new FakePort();
    const ch = new LinkChannel(port);
    for (let i = 0; i < MAX_IN_FLIGHT; i++) ch.request(messages(), recorder().sink);
    expect(ch.inFlight).toBe(8);
    expect(ch.takes(messages())).toBe(false);
    const ninth = recorder();
    ch.request(messages(), ninth.sink);
    await Promise.resolve();
    expect(ninth.got.error).toBeInstanceOf(LinkLost);
    expect(port.frames().filter((f) => f.t === "req")).toHaveLength(8);
    // a body over 60,000 bytes is never sent on the channel
    expect(ch.takes({ ...messages(), method: "POST", body: "x".repeat(60_001) })).toBe(false);
  });

  it("cancel tells the Mac and drops anything that still arrives for it", () => {
    const port = new FakePort();
    const ch = new LinkChannel(port);
    const r = recorder();
    const cancel = ch.request(messages(), r.sink);
    const id = port.lastReqId();
    port.deliver(encodeHead(id, 200));
    cancel();
    expect(port.frames().at(-1)).toEqual({ t: "cancel", id });
    for (const f of encodeData(id, new TextEncoder().encode("late"))) port.deliver(f);
    port.deliver(encodeEnd(id));
    expect(r.got.body).toHaveLength(0);
    expect(r.got.ended).toBe(false);
    expect(ch.inFlight).toBe(0);
  });

  it("a channel that closes fails every open request with LinkLost, which the host may retry", () => {
    const port = new FakePort();
    const ch = new LinkChannel(port);
    const a = recorder();
    const b = recorder();
    ch.request(messages(), a.sink);
    ch.request(messages(), b.sink);
    port.deliver(encodeHead(port.lastReqId(), 200));
    port.close();
    expect(a.got.error).toBeInstanceOf(LinkLost);
    expect(b.got.error).toBeInstanceOf(LinkLost);
    expect(ch.open).toBe(false);
  });

  it("an err frame, a body before its head and a second head each end the request with LinkLost", () => {
    const port = new FakePort();
    const ch = new LinkChannel(port);
    const errd = recorder();
    ch.request(messages(), errd.sink);
    port.deliver(encodeErr(port.lastReqId()));
    expect(errd.got.error).toBeInstanceOf(LinkLost);

    const early = recorder();
    ch.request(messages(), early.sink);
    for (const f of encodeData(port.lastReqId(), new Uint8Array([1]))) port.deliver(f);
    expect(early.got.error).toBeInstanceOf(LinkLost);

    const twice = recorder();
    ch.request(messages(), twice.sink);
    port.deliver(encodeHead(port.lastReqId(), 200));
    port.deliver(encodeHead(port.lastReqId(), 200));
    expect(twice.got.error).toBeInstanceOf(LinkLost);
    expect(ch.inFlight).toBe(0);
  });

  it("probe: alive when the Mac answers its 404 path, dead when nothing comes back in time", async () => {
    vi.useFakeTimers();
    const port = new FakePort();
    const ch = new LinkChannel(port);
    const alive = ch.probe(2500);
    const probe = port.frames().at(-1)!;
    expect(probe).toMatchObject({ t: "req", m: "GET", p: PROBE_PATH });
    port.deliver(encodeHead(probe.id as number, 404));
    await expect(alive).resolves.toBe(true);

    const dead = ch.probe(2500);
    const silent = port.frames().at(-1)!;
    await vi.advanceTimersByTimeAsync(2500);
    await expect(dead).resolves.toBe(false);
    expect(port.frames().at(-1)).toEqual({ t: "cancel", id: silent.id });

    port.close();
    await expect(ch.probe(2500)).resolves.toBe(false);
  });
});
