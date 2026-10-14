import { afterEach, describe, expect, it, vi } from "vitest";
import { Flow, spendSecret, type Phase } from "../flow";
import { macLogin, pairDevice, readFragment, takeFragment } from "../pair";
import { gradePref } from "../prefs";

const SECRET = "CANARY-NOT-A-SECRET-pair-7f3c";
const COOKIE_TOKEN = "CANARY-NOT-A-TOKEN-cookie-7f3c";

function history(state: unknown = { at: 1 }) {
  const calls: unknown[][] = [];
  return { calls, state, replaceState: (...args: unknown[]) => calls.push(args) };
}

/** Every write the page makes to its own storage and cookies, for the absence checks. */
function watchStorage() {
  const writes: string[] = [];
  const store = () => {
    const data = new Map<string, string>();
    return {
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => {
        writes.push(`${k}=${v}`);
        data.set(k, String(v));
      },
      removeItem: (k: string) => data.delete(k),
      clear: () => data.clear(),
      key: () => null,
      get length() {
        return data.size;
      },
    };
  };
  vi.stubGlobal("localStorage", store());
  vi.stubGlobal("sessionStorage", store());
  vi.stubGlobal("document", {
    get cookie() {
      return "";
    },
    set cookie(v: string) {
      writes.push(`cookie:${v}`);
    },
  });
  return writes;
}

/** Does any write hold one of these values? (The check the absence tests rely on.) */
const leaked = (writes: string[], ...values: string[]) => writes.filter((w) => values.some((v) => w.includes(v)));

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("the fragment's one-time secret", () => {
  it("fragments cleared: #p= and #login= leave the address bar at once, path and query kept", () => {
    const h = history();
    expect(takeFragment({ hash: `#p=${SECRET}`, pathname: "/", search: "" }, h)).toEqual({ kind: "pair", secret: SECRET });
    expect(h.calls).toEqual([[{ at: 1 }, "", "/"]]);

    const h2 = history(null);
    expect(takeFragment({ hash: `#login=${SECRET}`, pathname: "/chat", search: "?a=1" }, h2)).toEqual({
      kind: "login",
      code: SECRET,
    });
    expect(h2.calls).toEqual([[null, "", "/chat?a=1"]]);
  });

  it("fragments cleared even when malformed; any other fragment is left alone", () => {
    const h = history();
    expect(takeFragment({ hash: "#p=<script>", pathname: "/", search: "" }, h)).toBeNull();
    expect(h.calls).toHaveLength(1);
    const h2 = history();
    expect(takeFragment({ hash: "#login=", pathname: "/", search: "" }, h2)).toBeNull();
    expect(h2.calls).toHaveLength(1);
    const h3 = history();
    expect(takeFragment({ hash: "#top", pathname: "/", search: "" }, h3)).toBeNull();
    expect(takeFragment({ hash: "", pathname: "/", search: "" }, h3)).toBeNull();
    expect(h3.calls).toHaveLength(0);
    expect(readFragment("#p=short")).toBeNull();
  });

  it("no token in localStorage or document.cookie: pairing from #p= stores nothing secret in the page", async () => {
    const writes = watchStorage();
    const posted: { url: string; body: string }[] = [];
    const fetchFn = async (url: string, init: RequestInit = {}) => {
      posted.push({ url, body: typeof init.body === "string" ? init.body : "" });
      if (url === "/api/web/pair") {
        // the Mac sets the device cookie itself, HttpOnly: the page never sees its value
        return new Response('{"ok":true}', {
          status: 200,
          headers: { "content-type": "application/json", "set-cookie": `eve_dev=${COOKIE_TOKEN}; HttpOnly; Secure` },
        });
      }
      return new Response('{"state":"READY","openrouter":true,"voice":true,"lan":true}', { status: 200 });
    };

    const fragment = takeFragment({ hash: `#p=${SECRET}`, pathname: "/", search: "" }, history());
    const phases: Phase[] = [];
    const flow = new Flow({ where: "lan", onPhase: (p) => phases.push(p), fetch: fetchFn });
    await flow.boot(spendSecret(fragment, fetchFn));
    gradePref.set("think"); // the page's one stored choice, which is never a secret
    gradePref.set("auto");

    expect(phases.at(-1)).toEqual({ name: "chat" });
    expect(posted[0]).toEqual({ url: "/api/web/pair", body: JSON.stringify({ secret: SECRET }) });
    expect(posted.filter((p) => p.body.includes(SECRET))).toHaveLength(1); // posted once, then dropped
    expect(writes.length).toBeGreaterThan(0); // the watch saw the page's own storage writes
    expect(leaked(writes, SECRET, COOKIE_TOKEN, "eve_dev", "eve_mac")).toEqual([]);
    expect(writes.filter((w) => w.startsWith("cookie:"))).toEqual([]);

    // positive control: the same check catches a page that did keep the secret
    localStorage.setItem("pairing", SECRET);
    expect(leaked(writes, SECRET, COOKIE_TOKEN)).toHaveLength(1);
  });

  it("the Mac's sign-in code is posted once to /api/mac/login and kept nowhere", async () => {
    const writes = watchStorage();
    const bodies: string[] = [];
    const ok = await macLogin(SECRET, async (url, init = {}) => {
      bodies.push(`${url} ${String(init.body)}`);
      return new Response('{"ok":true}', { status: 200 });
    });
    expect(ok).toEqual({ ok: true });
    expect(bodies).toEqual([`/api/mac/login ${JSON.stringify({ code: SECRET })}`]);
    expect(leaked(writes, SECRET)).toEqual([]);
  });

  it("a code that fails says what to do; a typed code is six digits before anything is sent", async () => {
    let sent = 0;
    const refuse = async () => {
      sent += 1;
      return new Response(JSON.stringify({ ok: false, error: "That code expired. Make a new one on the Mac." }), { status: 403 });
    };
    expect(await pairDevice({ code: "12345" }, refuse)).toMatchObject({ ok: false, message: "The code is six digits." });
    expect(sent).toBe(0);
    expect(await pairDevice({ code: "123456" }, refuse)).toEqual({
      ok: false,
      status: 403,
      message: "That code expired. Make a new one on the Mac.",
    });
    const locked = await pairDevice({ code: "123456" }, async () => new Response("{}", { status: 429 }));
    expect(locked).toMatchObject({ ok: false, status: 429, message: "Too many tries. Make a new code on the Mac." });
    const generic = await pairDevice({ secret: SECRET }, async () => new Response("", { status: 410 }));
    expect(generic.ok).toBe(false);
    if (!generic.ok) expect(generic.message).toMatch(/Add a phone/);
  });
});
