// What main.tsx settles before anything renders, kept apart from the page so it can be tested without a browser.
//
// * Which of the Mac's doors served the page. Each speaks one protocol (docs/plans/eve-lite.md, M4): the loopback
//   door (LocalServer) only plain http, the LAN door (LanServer) only TLS. So the protocol decides, never the
//   name: the LAN door's certificate names localhost and 127.0.0.1 too (M5), and https://127.0.0.1 is the LAN door
//   reached from the Mac itself.
// * Whether the Mac's page moves to localhost first. `eve-bridge web open` signs the Mac's browser in at
//   localhost, and a browser keeps a cookie per host name, so eve_mac is localhost's alone: http://127.0.0.1 and
//   http://[::1] go to http://localhost. An https page never moves: its port speaks only TLS.
// * crypto.randomUUID where the browser has none. OpenUI calls it, and a browser offers it only in a secure
//   context (spike S2 checks that a LAN page still is one after its certificate warning was clicked through). The
//   stand-in takes its bytes from getRandomValues, which every context has, and never calls crypto.randomUUID.
//   nativeBridge.uuid() does call it whenever there is one, so it cannot be the stand-in: it would call itself
//   until the stack ran out.

import type { Where } from "./api";

/** The parts of the page's address these read (window.location has them, and so does a URL). */
export interface PageAddress {
  readonly protocol: string;
  readonly hostname: string;
  readonly port: string;
  readonly pathname: string;
  readonly search: string;
}

/** The Mac's own page is plain http (the loopback door); every https page is the LAN door's, whatever its name. */
export function whereFrom(protocol: string): Where {
  return protocol === "https:" ? "lan" : "mac";
}

/** The loopback addresses the Mac's door also answers on. Its cookie is not theirs. */
const LOOPBACK_ADDRESSES = new Set(["127.0.0.1", "[::1]"]);

/** The same page at localhost, for the Mac's page opened at http://127.0.0.1 or http://[::1]; null for any other. */
export function macHome(page: PageAddress): string | null {
  if (page.protocol !== "http:" || !LOOPBACK_ADDRESSES.has(page.hostname)) return null;
  return `http://localhost${page.port ? `:${page.port}` : ""}${page.pathname}${page.search}`;
}

/** A random (version 4) UUID, its bytes from `fill` alone. */
export function randomUuid(fill: (bytes: Uint8Array<ArrayBuffer>) => unknown): string {
  const b = new Uint8Array(16);
  fill(b);
  b[6] = (b[6]! & 0x0f) | 0x40;
  b[8] = (b[8]! & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** Give `c` a randomUUID when the browser offers none. Returns whether it had to. */
export function ensureRandomUuid(c: Crypto | undefined = globalThis.crypto): boolean {
  if (!c || typeof c.randomUUID === "function" || typeof c.getRandomValues !== "function") return false;
  const randomUUID = () => randomUuid((bytes) => c.getRandomValues(bytes));
  Object.defineProperty(c, "randomUUID", { value: randomUUID, configurable: true, writable: true });
  return true;
}
