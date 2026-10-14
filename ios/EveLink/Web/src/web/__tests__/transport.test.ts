import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import { encodeEnd, encodeHead } from "../framing";
import { LinkLost, type LinkRequest } from "../linkChannel";
import { NoWebRtc, type RtcPeer } from "../rtc";
import { FIRST_WAIT_MS, RETRY_MAX_MS, RETRY_MIN_MS, Transport, type LinkStatus } from "../transport";
import { fakeFetch, FakePeer, recorder } from "./fakes";

const poll: LinkRequest = {
  method: "GET",
  path: "/api/link/messages",
  query: "after=c1&wait=25",
  headers: { accept: "application/json" },
  body: "",
};

function setup(opts: { loopback?: boolean; visible?: () => boolean } = {}) {
  const peers: FakePeer[] = [];
  const plan: (() => Promise<FakePeer>)[] = [];
  const connectTimes: number[] = [];
  const unauthorized = vi.fn();
  const f = fakeFetch();
  const transport = new Transport({
    loopback: opts.loopback ?? false,
    connect: () => {
      connectTimes.push(Date.now());
      const next = plan.shift();
      if (next) return next() as unknown as Promise<RtcPeer>;
      const peer = new FakePeer();
      peers.push(peer);
      return Promise.resolve(peer as unknown as RtcPeer);
    },
    fetch: f.fetch,
    onUnauthorized: unauthorized,
    ...(opts.visible ? { isVisible: opts.visible } : {}),
  });
  const statuses: LinkStatus[] = [];
  transport.onStatus((s) => statuses.push(s));
  return { transport, peers, plan, connectTimes, unauthorized, f, statuses };
}

const fails = () => Promise.reject(new Error("no answer"));

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(0);
});

afterEach(() => {
  vi.useRealTimers();
});

describe("transport: the DataChannel first, HTTPS when it is blocked", () => {
  it("waits up to 4 s for the link to open (the 4 s fallback), then sends chat over HTTPS", async () => {
    const t = setup();
    t.transport.start();
    expect(t.transport.status).toBe("connecting");
    const r = recorder();
    t.transport.request(poll, r.sink);
    await vi.advanceTimersByTimeAsync(FIRST_WAIT_MS - 1);
    expect(t.f.calls).toHaveLength(0);
    expect(t.peers[0]!.port.sent).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(t.transport.status).toBe("fallback");
    expect(t.f.calls.map((c) => c.url)).toEqual(["/api/web/link/messages?after=c1&wait=25"]);
    expect(t.f.calls[0]!.init).toMatchObject({ method: "GET", credentials: "same-origin" });
  });

  it("a link that opens within the 4 s carries the requests that waited for it", async () => {
    const t = setup();
    t.transport.start();
    const r = recorder();
    t.transport.request(poll, r.sink);
    await vi.advanceTimersByTimeAsync(1500);
    t.peers[0]!.openLink();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.transport.status).toBe("direct");
    expect(t.peers[0]!.port.frames()).toMatchObject([{ t: "req", m: "GET", p: "/api/link/messages", q: "after=c1&wait=25" }]);
    expect(t.f.calls).toHaveLength(0);
  });

  it("switches back: WebRTC is retried 5 s after it failed, and new requests return to the DataChannel", async () => {
    const t = setup();
    t.plan.push(fails);
    t.transport.start();
    await vi.advanceTimersByTimeAsync(0);
    // a quick failure does not hold chat for the whole 4 s
    expect(t.transport.status).toBe("fallback");
    t.transport.request(poll, recorder().sink);
    expect(t.f.calls).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(RETRY_MIN_MS - 1);
    expect(t.connectTimes).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(t.connectTimes).toEqual([0, RETRY_MIN_MS]);
    t.peers[0]!.openLink();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.transport.status).toBe("direct");

    t.transport.request(poll, recorder().sink);
    expect(t.f.calls).toHaveLength(1); // no new fetch
    expect(t.peers[0]!.port.frames()).toHaveLength(1);
    expect(t.statuses).toEqual(["fallback", "direct"]);
  });

  it("backs off from 5 s, doubling, to at most 60 s", async () => {
    const t = setup();
    for (let i = 0; i < 8; i++) t.plan.push(fails);
    t.transport.start();
    await vi.advanceTimersByTimeAsync(400_000);
    const gaps = t.connectTimes.slice(1).map((at, i) => at - t.connectTimes[i]!);
    expect(gaps.slice(0, 7)).toEqual([5000, 10_000, 20_000, 40_000, RETRY_MAX_MS, RETRY_MAX_MS, RETRY_MAX_MS]);
  });

  it("an attempt that never opens is given up after 15 s and retried", async () => {
    const t = setup();
    t.transport.start();
    await vi.advanceTimersByTimeAsync(15_000);
    expect(t.peers[0]!.closed).toBe(true);
    await vi.advanceTimersByTimeAsync(RETRY_MIN_MS);
    expect(t.connectTimes).toEqual([0, 20_000]);
  });

  it("a link that drops fails its open request with LinkLost; the next request goes over HTTPS", async () => {
    const t = setup();
    t.transport.start();
    t.peers[0]!.openLink();
    await vi.advanceTimersByTimeAsync(0);
    const open = recorder();
    t.transport.request(poll, open.sink);
    const id = t.peers[0]!.port.lastReqId();
    t.peers[0]!.port.deliver(encodeHead(id, 200));
    t.peers[0]!.die();
    expect(open.got.error).toBeInstanceOf(LinkLost);
    expect(t.transport.status).toBe("fallback");
    t.transport.request(poll, recorder().sink);
    expect(t.f.calls).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(RETRY_MIN_MS);
    expect(t.connectTimes).toHaveLength(2);
  });

  it("a request the channel cannot take goes over HTTPS even while direct", async () => {
    const t = setup();
    t.transport.start();
    t.peers[0]!.openLink();
    await vi.advanceTimersByTimeAsync(0);
    const big: LinkRequest = { method: "POST", path: "/api/link/turn", query: "", headers: {}, body: "x".repeat(60_001) };
    const handle = t.transport.request(big, recorder().sink);
    await expect(handle.via).resolves.toBe("fetch");
    expect(t.f.calls[0]).toMatchObject({ url: "/api/web/link/turn", init: { method: "POST" } });
    const small = t.transport.request(poll, recorder().sink, { via: "fetch" });
    await expect(small.via).resolves.toBe("fetch");
  });

  it("the Mac's own page never connects for chat: every request is a loopback fetch", async () => {
    const t = setup({ loopback: true });
    t.transport.start();
    expect(t.transport.status).toBe("loopback");
    const r = recorder();
    t.f.answer(() => new Response('{"messages":[],"cursor":"c","busy":false}', { status: 200 }));
    t.transport.request(poll, r.sink);
    await vi.advanceTimersByTimeAsync(0);
    expect(t.connectTimes).toHaveLength(0);
    expect(r.got.status).toBe(200);
    expect(r.got.ended).toBe(true);
    expect(r.text()).toContain('"cursor":"c"');
  });

  it("wake: a link that no longer answers a probe is dropped and WebRTC is tried again at once", async () => {
    const t = setup();
    t.transport.start();
    t.peers[0]!.openLink();
    await vi.advanceTimersByTimeAsync(0);
    t.transport.wake();
    await vi.advanceTimersByTimeAsync(2500); // the probe's time is up: nothing came back
    expect(t.peers[0]!.closed).toBe(true);
    expect(t.connectTimes).toEqual([0, 2500]);

    // a link that answers the probe stays
    t.peers[1]!.openLink();
    await vi.advanceTimersByTimeAsync(0);
    t.transport.wake();
    const probe = t.peers[1]!.port.frames().at(-1)!;
    t.peers[1]!.port.deliver(encodeHead(probe.id as number, 404));
    t.peers[1]!.port.deliver(encodeEnd(probe.id as number));
    await vi.advanceTimersByTimeAsync(5000);
    expect(t.peers[1]!.closed).toBe(false);
    expect(t.transport.status).toBe("direct");
  });

  it("a hidden page makes no new offer until it shows again", async () => {
    let visible = true;
    const t = setup({ visible: () => visible });
    t.plan.push(fails);
    t.transport.start();
    await vi.advanceTimersByTimeAsync(0);
    visible = false;
    await vi.advanceTimersByTimeAsync(120_000);
    expect(t.connectTimes).toHaveLength(1);
    visible = true;
    t.transport.wake();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.connectTimes).toHaveLength(2);
  });

  it("a browser with no WebRTC chats over HTTPS for good, without retrying", async () => {
    const t = setup();
    t.plan.push(() => Promise.reject(new NoWebRtc()));
    t.transport.start();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(t.transport.status).toBe("fallback");
    expect(t.connectTimes).toHaveLength(1);
  });

  it("signaling answered 401: the sign-in is gone, and WebRTC is not retried", async () => {
    const t = setup();
    t.plan.push(() => Promise.reject(new ApiError(401, "no")));
    t.transport.start();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(t.unauthorized).toHaveBeenCalledTimes(1);
    expect(t.connectTimes).toHaveLength(1);
  });

  it("stopped and started again at once: the old attempt's end changes nothing, the new one connects", async () => {
    const t = setup();
    let failOld!: (e: Error) => void;
    t.plan.push(() => new Promise<FakePeer>((_, reject) => (failOld = reject)));
    t.transport.start();
    t.transport.stop();
    t.transport.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.connectTimes).toHaveLength(2); // a fresh attempt, not the stopped one's promise
    failOld(new Error("aborted"));
    await vi.advanceTimersByTimeAsync(0);
    expect(t.transport.status).toBe("connecting"); // the old failure did not end the new first window
    t.peers[0]!.openLink();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.transport.status).toBe("direct");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(t.connectTimes).toHaveLength(2);
  });

  it("a request cancelled while it waits for the link is never sent", async () => {
    const t = setup();
    t.transport.start();
    const handle = t.transport.request(poll, recorder().sink);
    handle.cancel();
    await vi.advanceTimersByTimeAsync(FIRST_WAIT_MS);
    expect(t.f.calls).toHaveLength(0);
    expect(t.peers[0]!.port.sent).toHaveLength(0);
  });

  it("voice gets the chat's peer on a phone, and a peer of its own on the Mac that the call lets go", async () => {
    const phone = setup();
    phone.transport.start();
    phone.peers[0]!.openLink();
    await vi.advanceTimersByTimeAsync(0);
    await expect(phone.transport.peer()).resolves.toBe(phone.peers[0]);

    const mac = setup({ loopback: true });
    const asked = mac.transport.peer();
    await vi.advanceTimersByTimeAsync(0);
    mac.peers[0]!.openLink();
    await expect(asked).resolves.toBe(mac.peers[0]);
    mac.transport.releaseVoice();
    expect(mac.peers[0]!.closed).toBe(true);
  });
});
