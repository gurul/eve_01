import { describe, expect, it } from "vitest";
import {
  api,
  ApiError,
  connectLink,
  noCredit,
  readApps,
  readDevices,
  readOnboard,
  readPairWindow,
  readRtcAnswer,
  readStatus,
  setupDone,
  UNREACHABLE,
} from "../api";
import { SseEvents } from "../sse";
import { saveName } from "../windows";

describe("reading the Mac's answers", () => {
  it("odd or missing fields read as off, never as a crash", () => {
    for (const raw of [null, undefined, "x", 3, [], {}]) {
      expect(readOnboard(raw)).toEqual({
        openrouter: false,
        openrouterRejected: false,
        credit: null,
        funded: null,
        composio: false,
        composioSkipped: false,
        restarting: false,
        error: "",
      });
      expect(readStatus(raw)).toMatchObject({ voice: false, lan: false, localNetwork: "unknown", credit: null, funded: null });
      expect(readApps(raw)).toEqual({ composio: false, apps: [] });
      expect(readDevices(raw)).toEqual([]);
    }
    expect(readOnboard({ openrouter: "yes", credit: "5" })).toMatchObject({ openrouter: false, credit: null });
  });

  it("credit is US dollars, a number or {usd}", () => {
    expect(readOnboard({ credit: 4.97 }).credit).toBe(4.97);
    expect(readOnboard({ credit: { usd: 0 } }).credit).toBe(0);
    expect(readStatus({ credit: { remaining: 1.5 } }).credit).toBe(1.5);
  });

  it("credit as onboarding.credit() answers it: usd null and funded false is no credit; not known is not", () => {
    // WP-5's answers (bridge/tests/test_onboarding.py pins each), then bare figures: [answer, usd, funded, no credit]
    const answers: [unknown, number | null, boolean | null, boolean][] = [
      [{ ok: true, usd: 7.5, funded: true }, 7.5, true, false],
      [{ ok: true, usd: 0, funded: false }, 0, false, true],
      [{ ok: true, usd: 1.25, funded: true }, 1.25, true, false],
      [{ ok: true, usd: null, funded: false }, null, false, true], // an OAuth key; the account never bought credits
      [{ ok: true, usd: null, funded: null }, null, null, false],
      [{ ok: false, usd: null, funded: null, error: "HTTP 401" }, null, null, false],
      [4.97, 4.97, null, false],
      [0, 0, null, true],
      [null, null, null, false],
    ];
    for (const [answer, usd, funded, empty] of answers) {
      for (const read of [readOnboard({ openrouter: true, credit: answer }), readStatus({ credit: answer })]) {
        expect([read.credit, read.funded, noCredit(read)]).toEqual([usd, funded, empty]);
      }
    }
    // A funded beside the figure (a door that flattened the answer) is read too; a figure decides over it.
    expect(readOnboard({ credit: null, funded: false })).toMatchObject({ credit: null, funded: false });
    expect(noCredit(readOnboard({ credit: null, funded: false }))).toBe(true);
    expect(noCredit(readStatus({ credit: 2, funded: false }))).toBe(false);
    // Anything but a boolean is not known.
    expect(readStatus({ credit: { usd: null, funded: "false" } })).toMatchObject({ credit: null, funded: null });
    expect(readStatus({ credit: { usd: null, funded: 0 } }).funded).toBeNull();
  });

  it("setup is done with a key and Composio signed in or skipped", () => {
    expect(setupDone(readOnboard({ openrouter: true, composio: true }))).toBe(true);
    expect(setupDone(readOnboard({ openrouter: true, composio_skipped: true }))).toBe(true);
    expect(setupDone(readOnboard({ openrouter: true }))).toBe(false);
    expect(setupDone(readOnboard({ composio: true }))).toBe(false);
  });

  it("a turned-down key reads as rejected, and is not 'done' so setup shows the re-sign-in", () => {
    expect(readOnboard({ openrouter: true, openrouter_rejected: true }).openrouterRejected).toBe(true);
    expect(readStatus({ openrouter: true, openrouter_rejected: true }).openrouterRejected).toBe(true);
    expect(readOnboard({ openrouter: true }).openrouterRejected).toBe(false);
    expect(setupDone(readOnboard({ openrouter: true, composio_skipped: true, openrouter_rejected: true }))).toBe(false);
  });

  it("a pairing window: two labelled codes, the six digits and when it closes; QR codes are data: URIs only", () => {
    const listed = readPairWindow(
      {
        code: "042137",
        expires_in: 300,
        links: [
          { label: "iPhone or iPad", url: "https://mac.local:48781/#p=abc", qr: "data:image/svg+xml;base64,PHN2Zz4=" },
          { label: "Android or other", url: "https://192.168.1.9:48781/#p=abc", qr: "https://evil.example/qr.png" },
          { label: "Not https", url: "http://192.168.1.9:48781/#p=abc", qr: "" },
        ],
      },
      1000,
    );
    expect(listed.code).toBe("042137");
    expect(listed.expiresAt).toBe(301_000);
    expect(listed.links.map((l) => [l.label, l.qr === "" ? "no qr" : "qr"])).toEqual([
      ["iPhone or iPad", "qr"],
      ["Android or other", "no qr"],
    ]);
    const flat = readPairWindow({
      code: "1234567",
      expires: 2000,
      url_apple: "https://mac.local:48781/#p=x",
      qr_apple: "data:image/svg+xml,<svg/>",
      url_other: "https://10.0.0.2:48781/#p=x",
    });
    expect(flat.code).toBe("");
    expect(flat.expiresAt).toBe(2_000_000);
    expect(flat.links.map((l) => l.label)).toEqual(["iPhone or iPad", "Android or other"]);
  });

  it("devices and apps: well-formed entries only", () => {
    expect(
      readDevices({
        devices: [
          { id: "d1", label: "iPhone", host: "mac.local", created: 1, last_seen: 2, current: false },
          { label: "no id" },
          { id: "d2", current: true },
        ],
      }),
    ).toEqual([
      { id: "d1", label: "iPhone", host: "mac.local", created: 1, lastSeen: 2, current: false },
      { id: "d2", label: "A device", host: "", created: null, lastSeen: null, current: true },
    ]);
    expect(
      readApps({ composio: true, apps: [{ toolkit: "GMAIL", name: "Gmail", connected: true }, { toolkit: "../x", name: "x" }] }),
    ).toEqual({ composio: true, apps: [{ toolkit: "gmail", name: "Gmail", connected: true }] });
  });

  it("a Connect Link is https on composio.dev or a subdomain, nothing else", () => {
    expect(connectLink({ url: "https://connect.composio.dev/link/abc" })).toBe("https://connect.composio.dev/link/abc");
    expect(connectLink({ url: "https://composio.dev/x" })).toBe("https://composio.dev/x");
    for (const url of [
      "http://connect.composio.dev/link",
      "https://composio.dev.evil.example/x",
      "https://evilcomposio.dev/x",
      "javascript:alert(1)",
      "",
    ]) {
      expect(connectLink({ url })).toBeNull();
    }
  });

  it("an RTC answer is SDP or nothing", () => {
    expect(readRtcAnswer({ sdp: "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n", voice: true })).toEqual({
      sdp: "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n",
      voice: true,
    });
    expect(readRtcAnswer({ sdp: "<html>" })).toBeNull();
    expect(readRtcAnswer({})).toBeNull();
  });
});

describe("api()", () => {
  it("same-origin JSON: the error's own line on a refusal, status 0 when eve can't be reached", async () => {
    let seen: RequestInit | undefined;
    const ok = await api("POST", "/api/web/lan", { on: false }, async (_url, init) => {
      seen = init;
      return new Response('{"ok":true,"lan":false}', { status: 200 });
    });
    expect(ok).toEqual({ ok: true, lan: false });
    expect(seen).toMatchObject({ method: "POST", credentials: "same-origin", body: '{"on":false}' });
    expect((seen!.headers as Record<string, string>)["Content-Type"]).toBe("application/json");

    const refused = await api("GET", "/x", undefined, async () => new Response('{"ok":false,"error":"Nope."}', { status: 403 })).catch(
      (e: unknown) => e,
    );
    expect(refused).toEqual(new ApiError(403, "Nope."));
    const down = await api("GET", "/x", undefined, async () => {
      throw new TypeError("refused");
    }).catch((e: unknown) => e);
    expect(down).toEqual(new ApiError(0, UNREACHABLE));
  });
});

describe("SseEvents", () => {
  it("passes whole events on, counts data events only, and skips the ones already shown", () => {
    const first = new SseEvents();
    expect(first.push("data: 1\n\n: ping\n\ndata: 2")).toBe("data: 1\n\n: ping\n\n");
    expect(first.delivered).toBe(1);
    expect(first.push("\n\n")).toBe("data: 2\n\n");
    expect(first.delivered).toBe(2);

    const again = new SseEvents(first.delivered);
    expect(again.push(": ping\n\ndata: 1\n\ndata: 2\n\ndata: 3\n\n")).toBe("data: 3\n\n");
    expect(again.delivered).toBe(3);
    expect(again.push("data: [DONE]")).toBe("");
    expect(again.end()).toBe("data: [DONE]");
  });
});

describe("saveName", () => {
  it("adds the type's extension when the name has none, and cleans path characters", () => {
    expect(saveName("Photo", "image/jpeg")).toBe("Photo.jpg");
    expect(saveName("scan.png", "image/png")).toBe("scan.png");
    expect(saveName("a/b:c", "application/pdf")).toBe("a b c.pdf");
    expect(saveName("", "")).toBe("eve file");
  });
});
