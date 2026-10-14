// The one-time secrets that reach this page in its URL fragment (a fragment is never sent in a request):
//
//   #p=<secret>     a phone or laptop pairing, from the QR code the Mac shows (the LAN door)
//   #login=<code>   the Mac's own browser, from `eve-bridge web open` (the loopback door)
//
// The fragment is cleared with history.replaceState before anything else runs, so the secret is not left in the
// address bar, a bookmark or a shared link. It is posted once and dropped. What comes back is an HttpOnly cookie
// (eve_dev or eve_mac) that this page cannot read: no token is ever held in JavaScript or in storage.

import { api, ApiError, UNREACHABLE, type Fetch } from "./api";

export type Fragment = { kind: "pair"; secret: string } | { kind: "login"; code: string };

/** A 128-bit secret or code, base64url or hex. */
const SECRET = /^[A-Za-z0-9_-]{16,128}$/;
/** The 6-digit code the Mac shows beside its QR codes. */
export const PAIR_CODE = /^\d{6}$/;

export interface UrlLike {
  readonly hash: string;
  readonly pathname: string;
  readonly search: string;
}

export interface HistoryLike {
  readonly state: unknown;
  replaceState(data: unknown, unused: string, url?: string | null): void;
}

/** What a fragment carries, or null. A malformed secret is null too (and still cleared by takeFragment). */
export function readFragment(hash: string): Fragment | null {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  const secret = params.get("p");
  if (secret !== null) return SECRET.test(secret) ? { kind: "pair", secret } : null;
  const code = params.get("login");
  if (code !== null) return SECRET.test(code) ? { kind: "login", code } : null;
  return null;
}

/** Whether the fragment holds one of the secrets' keys at all (well formed or not). */
function holdsSecret(hash: string): boolean {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  return params.has("p") || params.has("login");
}

/** Read the fragment's secret and clear it from the URL at once. Call this before anything else runs. */
export function takeFragment(location: UrlLike, history: HistoryLike): Fragment | null {
  const hash = location.hash;
  if (!hash || !holdsSecret(hash)) return null;
  const found = readFragment(hash);
  history.replaceState(history.state, "", location.pathname + location.search);
  return found;
}

export type Outcome = { ok: true } | { ok: false; status: number; message: string };

const PAIR_FAILED =
  "That code didn't work. Make a new one on the Mac: Settings, Phone access, Add a phone.";

function failed(e: unknown, fallback: string): Outcome {
  if (e instanceof ApiError) {
    if (e.status === 0) return { ok: false, status: 0, message: UNREACHABLE };
    if (e.status === 429) return { ok: false, status: 429, message: "Too many tries. Make a new code on the Mac." };
    const generic = e.message === `eve answered ${e.status}.`;
    return { ok: false, status: e.status, message: generic ? fallback : e.message };
  }
  return { ok: false, status: 0, message: fallback };
}

/** Pair this phone or laptop: the QR code's secret, or the 6-digit code typed in. Sets eve_dev. */
export async function pairDevice(proof: { secret: string } | { code: string }, fetchFn?: Fetch): Promise<Outcome> {
  let body: { secret: string } | { code: string };
  if ("secret" in proof) {
    body = { secret: proof.secret };
  } else {
    if (!PAIR_CODE.test(proof.code)) return { ok: false, status: 400, message: "The code is six digits." };
    body = { code: proof.code };
  }
  try {
    await api("POST", "/api/web/pair", body, fetchFn);
    return { ok: true };
  } catch (e) {
    return failed(e, PAIR_FAILED);
  }
}

/** Sign the Mac's own browser in with the one-time code `eve-bridge web open` passed. Sets eve_mac. */
export async function macLogin(code: string, fetchFn?: Fetch): Promise<Outcome> {
  try {
    await api("POST", "/api/mac/login", { code }, fetchFn);
    return { ok: true };
  } catch (e) {
    return failed(e, "That sign-in link was already used. Open Eve from Applications again.");
  }
}
