import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { COOKIE_NOT_KEPT, Flow, NOT_SET_UP, RESTART_LATE_MS, spendSecret, type Phase } from "../flow";
import { DEVICE_UNPAIRED, MAC_SIGNED_OUT } from "../host";

type Route = (url: string, init: RequestInit) => Response | Promise<Response>;

function server(routes: Record<string, Route | Response[]>) {
  const seen: string[] = [];
  const fetchFn = async (url: string, init: RequestInit = {}) => {
    seen.push(`${init.method ?? "GET"} ${url}`);
    const route = routes[url];
    if (!route) throw new TypeError("connection refused");
    if (Array.isArray(route)) {
      const next = route.length > 1 ? route.shift()! : route[0]!;
      return next.clone();
    }
    return route(url, init);
  };
  return { fetchFn, seen };
}

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
const SECRET = "CANARY-NOT-A-SECRET-flow-7f3c";
const ONBOARD = "/onboard/state";
const STATUS = "/api/web/status";

function flow(where: "mac" | "lan", fetchFn: (url: string, init?: RequestInit) => Promise<Response>) {
  const phases: Phase[] = [];
  const f = new Flow({ where, onPhase: (p) => phases.push(p), fetch: fetchFn });
  return { f, phases, last: () => phases.at(-1) };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("what the page shows", () => {
  it("the Mac: setup until OpenRouter is in and Composio is signed in or skipped, then the chat", async () => {
    const s = server({
      [ONBOARD]: [
        json({ openrouter: false, composio: false }),
        json({ openrouter: true, credit: 0, composio: false }),
        json({ openrouter: true, credit: 4.97, composio: false, composio_skipped: true }),
      ],
    });
    const t = flow("mac", s.fetchFn);
    await t.f.decide();
    expect(t.last()).toMatchObject({ name: "setup", state: { openrouter: false } });
    await t.f.decide();
    expect(t.last()).toMatchObject({ name: "setup", state: { openrouter: true, credit: 0 } });
    await t.f.decide();
    expect(t.last()).toEqual({ name: "chat" });
  });

  it("the Mac without its cookie: open Eve from Applications", async () => {
    const s = server({ [ONBOARD]: [json({ ok: false, error: "sign in" }, 401)] });
    const t = flow("mac", s.fetchFn);
    await t.f.decide();
    expect(t.last()).toEqual({ name: "signin", note: "" });
  });

  it("the Mac's one-time code is spent first, once, and the state decides", async () => {
    const s = server({ "/api/mac/login": [json({ ok: true })], [ONBOARD]: [json({ openrouter: true, composio: true })] });
    const t = flow("mac", s.fetchFn);
    await t.f.boot(spendSecret({ kind: "login", code: SECRET }, s.fetchFn));
    expect(s.seen).toEqual(["POST /api/mac/login", "GET /onboard/state"]);
    expect(t.last()).toEqual({ name: "chat" });
  });

  it("a sign-in just written: waits while eve restarts (answers or not), then carries on by itself", async () => {
    let calls = 0;
    const s = server({
      [ONBOARD]: () => {
        calls += 1;
        if (calls === 1) return json({ openrouter: true, composio: false, restarting: true });
        if (calls < 5) throw new TypeError("connection refused"); // down while launchd restarts it
        return json({ openrouter: true, composio: true });
      },
    });
    const t = flow("mac", s.fetchFn);
    void t.f.decide();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.last()).toEqual({ name: "restarting", late: false });
    await vi.advanceTimersByTimeAsync(5000);
    expect(t.last()).toEqual({ name: "chat" });
  });

  it("a restart that takes long says so, and keeps waiting", async () => {
    const s = server({ [ONBOARD]: () => json({ openrouter: true, restarting: true }) });
    const t = flow("mac", s.fetchFn);
    void t.f.decide();
    await vi.advanceTimersByTimeAsync(RESTART_LATE_MS + 2000);
    expect(t.last()).toEqual({ name: "restarting", late: true });
  });

  it("a phone: paired goes to the chat; not paired asks for the code", async () => {
    const paired = flow("lan", server({ [STATUS]: [json({ state: "READY", openrouter: true })] }).fetchFn);
    await paired.f.decide();
    expect(paired.last()).toEqual({ name: "chat" });
    const fresh = flow("lan", server({ [STATUS]: [json({}, 401)] }).fetchFn);
    await fresh.f.decide();
    expect(fresh.last()).toEqual({ name: "pair", note: "" });
  });

  it("a phone that paired but whose next request had no cookie is told to trust eve's certificate", async () => {
    const s = server({ "/api/web/pair": [json({ ok: true })], [STATUS]: [json({}, 401)] });
    const t = flow("lan", s.fetchFn);
    await t.f.boot(spendSecret({ kind: "pair", secret: SECRET }, s.fetchFn));
    expect(t.last()).toEqual({ name: "pair", note: COOKIE_NOT_KEPT });
  });

  it("a pairing that failed shows why on the code screen", async () => {
    const s = server({
      "/api/web/pair": [json({ ok: false, error: "That code was already used." }, 403)],
      [STATUS]: [json({}, 401)],
    });
    const t = flow("lan", s.fetchFn);
    await t.f.boot(spendSecret({ kind: "pair", secret: SECRET }, s.fetchFn));
    expect(t.last()).toEqual({ name: "pair", note: "That code was already used." });
  });

  it("a phone before the Mac is set up waits for the Mac", async () => {
    const t = flow("lan", server({ [STATUS]: [json({ state: "NEEDS_OPENROUTER" })] }).fetchFn);
    await t.f.decide();
    expect(t.last()).toEqual({ name: "unreachable", message: NOT_SET_UP });
  });

  it("eve not answering is retried by itself, more slowly each time, until it answers", async () => {
    let up = false;
    const s = server({ [STATUS]: () => (up ? json({ state: "READY" }) : Promise.reject(new TypeError("refused"))) });
    const t = flow("lan", s.fetchFn);
    await t.f.decide();
    expect(t.last()).toMatchObject({ name: "unreachable" });
    await vi.advanceTimersByTimeAsync(2000 + 4000 + 8000);
    expect(s.seen).toHaveLength(4);
    up = true;
    await vi.advanceTimersByTimeAsync(15_000);
    expect(t.last()).toEqual({ name: "chat" });
  });

  it("a secret is posted again only while eve does not answer at all; a refusal is final", async () => {
    let tries = 0;
    const flaky = server({
      "/api/web/pair": () => {
        tries += 1;
        if (tries < 3) throw new TypeError("connection refused");
        return json({ ok: true });
      },
    });
    const waits: number[] = [];
    const sleep = async (ms: number) => {
      waits.push(ms);
    };
    expect(await spendSecret({ kind: "pair", secret: SECRET }, flaky.fetchFn, sleep)).toEqual({
      note: "",
      justPaired: true,
    });
    expect([tries, waits]).toEqual([3, [1500, 3000]]);

    let refusals = 0;
    const refusing = server({
      "/api/mac/login": () => {
        refusals += 1;
        return json({ ok: false, error: "That sign-in link was used." }, 401);
      },
    });
    const spent = await spendSecret({ kind: "login", code: SECRET }, refusing.fetchFn, sleep);
    expect(spent).toEqual({ note: "That sign-in link was used.", justPaired: false });
    expect(refusals).toBe(1);
  });

  it("a sign-in lost in the chat, or a sign-out, goes back to the right screen", () => {
    const mac = flow("mac", server({}).fetchFn);
    mac.f.authLost();
    expect(mac.last()).toEqual({ name: "signin", note: MAC_SIGNED_OUT });
    const phone = flow("lan", server({}).fetchFn);
    phone.f.authLost();
    expect(phone.last()).toEqual({ name: "pair", note: DEVICE_UNPAIRED });
    phone.f.signedOut();
    expect(phone.last()).toEqual({ name: "pair", note: "" });
  });
});
