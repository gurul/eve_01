import { afterEach, describe, expect, it, vi } from "vitest";
import { uuid } from "../../nativeBridge";
import { ensureRandomUuid, macHome, randomUuid, whereFrom } from "../boot";

/** A version 4 UUID, lower case. */
const V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/** The page's address as the browser parses it (window.location has the same parts). */
const page = (address: string) => new URL(address);

/** Node's own crypto, kept before a test puts a page's in its place. */
const nodeCrypto = globalThis.crypto;

/** crypto as a page outside a secure context has it: getRandomValues, and no randomUUID. */
function insecureCrypto(): Crypto {
  return { getRandomValues: (b: Uint8Array<ArrayBuffer>) => nodeCrypto.getRandomValues(b) } as unknown as Crypto;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("which of the Mac's doors served the page", () => {
  it("https://127.0.0.1 reads as lan, as do https://localhost and https://[::1]: the LAN door's certificate names them too", () => {
    for (const address of ["https://127.0.0.1:48781/", "https://localhost:48781/", "https://[::1]:48781/"]) {
      expect(whereFrom(page(address).protocol), address).toBe("lan");
    }
  });

  it("a phone's or a laptop's address reads as lan", () => {
    for (const address of ["https://Studio.local:48781/", "https://192.168.1.20:48790/chat"]) {
      expect(whereFrom(page(address).protocol), address).toBe("lan");
    }
  });

  it("http is the Mac's own page, at any of the loopback door's names", () => {
    for (const address of ["http://localhost:48780/", "http://127.0.0.1:48780/", "http://[::1]:48782/"]) {
      expect(whereFrom(page(address).protocol), address).toBe("mac");
    }
  });
});

describe("the Mac's page moves to localhost, where its cookie is", () => {
  it("http://127.0.0.1 and http://[::1] go to http://localhost on the same port, path and query kept", () => {
    expect(macHome(page("http://127.0.0.1:48780/"))).toBe("http://localhost:48780/");
    expect(macHome(page("http://[::1]:48785/chat?x=1"))).toBe("http://localhost:48785/chat?x=1");
    expect(macHome(page("http://127.0.0.1/"))).toBe("http://localhost/");
  });

  it("the redirect is for http pages only: https at a loopback name stays, its port speaks only TLS", () => {
    for (const address of [
      "https://127.0.0.1:48781/",
      "https://[::1]:48781/",
      "https://localhost:48781/",
      "https://192.168.1.20:48781/",
    ]) {
      expect(macHome(page(address)), address).toBeNull();
    }
    expect(macHome(page("http://localhost:48780/"))).toBeNull(); // already there
    expect(macHome({ protocol: "file:", hostname: "127.0.0.1", port: "", pathname: "/", search: "" })).toBeNull();
  });
});

describe("crypto.randomUUID where the browser offers none", () => {
  it("a version 4 UUID from the bytes alone, with the version and variant bits set", () => {
    expect(randomUuid((b) => b.fill(0))).toBe("00000000-0000-4000-8000-000000000000");
    expect(randomUuid((b) => b.fill(0xff))).toBe("ffffffff-ffff-4fff-bfff-ffffffffffff");
    expect(randomUuid((b) => nodeCrypto.getRandomValues(b))).toMatch(V4);
  });

  it("the stand-in never reads crypto.randomUUID, so nativeBridge.uuid() through it gives v4 UUIDs and does not recurse", () => {
    vi.stubGlobal("crypto", insecureCrypto());
    expect(typeof globalThis.crypto.randomUUID).toBe("undefined");
    expect(ensureRandomUuid()).toBe(true);
    expect(globalThis.crypto.randomUUID()).toMatch(V4);
    const ids = Array.from({ length: 1000 }, () => uuid()); // nativeBridge's, which calls crypto.randomUUID
    expect(ids.every((id) => V4.test(id))).toBe(true);
    expect(new Set(ids).size).toBe(1000);
    expect(ensureRandomUuid()).toBe(false); // already in place
  });

  it("control: nativeBridge.uuid put in as the stand-in calls itself until the stack runs out", () => {
    const c = insecureCrypto();
    vi.stubGlobal("crypto", c);
    Object.defineProperty(c, "randomUUID", { value: uuid, configurable: true });
    expect(() => uuid()).toThrow(RangeError);
  });

  it("a browser's own randomUUID is left alone; without getRandomValues or crypto nothing is put in", () => {
    const own = () => "00000000-0000-4000-8000-000000000001" as const;
    const c = Object.assign(insecureCrypto(), { randomUUID: own });
    expect(ensureRandomUuid(c)).toBe(false);
    expect(c.randomUUID).toBe(own);

    const bare = {} as Crypto;
    expect(ensureRandomUuid(bare)).toBe(false);
    expect("randomUUID" in bare).toBe(false);

    vi.stubGlobal("crypto", undefined);
    expect(ensureRandomUuid()).toBe(false);
  });
});
