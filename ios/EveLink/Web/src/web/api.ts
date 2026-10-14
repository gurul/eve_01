// The Mac's web door (lite_web.py; docs/plans/eve-lite.md, M4) as this page calls it. Same-origin fetch only: the
// device cookie (eve_mac on the Mac, eve_dev on a phone) is HttpOnly, so the page never holds a token. Every answer
// is read defensively: a missing or odd field reads as "off", never as a crash.
//
// The JSON each route answers, as read here:
//   POST /api/mac/login {code}        {ok}                 sets eve_mac
//   GET  /onboard/state               {openrouter, openrouter_rejected, credit, composio, composio_skipped, restarting, error?}
//   POST /onboard/composio/skip       {ok}
//   POST /api/web/pair/open           {code, expires | expires_in, links: [{label, url, qr}]}
//                                     (or url_apple, qr_apple, url_other, qr_other)
//   POST /api/web/pair {secret}|{code} {ok}                 sets eve_dev
//   GET  /api/web/devices             {devices: [{id, label, host, created, last_seen, current?}]}
//   POST /api/web/devices/revoke {id} {ok, revoked: [id]}  (the page revokes one id at a time, never its own)
//   POST /api/web/lan {on}            {ok, lan}
//   POST /api/web/rtc {sdp}           {sdp, voice}
//   GET  /api/web/apps                {composio, apps: [{toolkit, name, connected}]}
//   POST /api/web/apps/<toolkit>/connect   {ok, url}       a Composio Connect Link
//   GET  /api/web/status              {state, openrouter, openrouter_rejected, composio, voice, lan, local_network, credit}
//   POST /api/web/logout              {ok}
// `credit`, on both GETs, is onboarding.credit()'s answer passed on as it is: {ok, usd, funded, error?}.
//   usd     the US dollars the key can spend; null when OpenRouter gives no figure, as for an OAuth key with no limit
//   funded  whether a paid call can go through: false at $0, and on an account that never bought credits (which has
//           no figure); null when not known. Without a figure, funded false is what makes the page offer Add credits.
// A bare number reads as usd, and a top-level funded beside it is read too.
// Errors are {ok: false, error: "<a plain line>"} with the status. Times are unix seconds.

export type Where = "mac" | "lan";

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/** fetch, as this page uses it (always a same-origin path). Tests give a fake. */
export type Fetch = (path: string, init?: RequestInit) => Promise<Response>;

/** The page's own fetch, called on its global (an unbound window.fetch throws "Illegal invocation"). */
export const pageFetch: Fetch = (path, init) => globalThis.fetch(path, init);

export const UNREACHABLE = "Can't reach eve. Check that the Mac is awake and on this Wi-Fi.";

function said(data: unknown): string | undefined {
  if (!data || typeof data !== "object") return undefined;
  const error = (data as { error?: unknown }).error;
  return typeof error === "string" && error.trim() ? error.trim().slice(0, 300) : undefined;
}

/** One JSON call. Throws ApiError: status 0 when eve could not be reached at all. */
export async function api(
  method: "GET" | "POST",
  path: string,
  body?: unknown,
  fetchFn: Fetch = pageFetch,
  signal?: AbortSignal,
): Promise<unknown> {
  let res: Response;
  try {
    res = await fetchFn(path, {
      method,
      credentials: "same-origin",
      cache: "no-store",
      headers: method === "GET" ? { Accept: "application/json" } : { Accept: "application/json", "Content-Type": "application/json" },
      body: method === "GET" ? undefined : JSON.stringify(body ?? {}),
      signal,
    });
  } catch (e) {
    if (signal?.aborted) throw e;
    throw new ApiError(0, UNREACHABLE);
  }
  let data: unknown = null;
  const text = await res.text().catch(() => "");
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = null;
    }
  }
  if (!res.ok) throw new ApiError(res.status, said(data) ?? `eve answered ${res.status}.`);
  return data;
}

// ---- small readers -------------------------------------------------------------------------------------------

function obj(raw: unknown): Record<string, unknown> {
  return raw && typeof raw === "object" && !Array.isArray(raw) ? (raw as Record<string, unknown>) : {};
}

const bool = (v: unknown): boolean => v === true;
const flag = (v: unknown): boolean | null => (typeof v === "boolean" ? v : null);
const str = (v: unknown, max = 500): string => (typeof v === "string" ? v.slice(0, max) : "");
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);

/** US dollars left: a number, or {usd} / {remaining}; null when unknown. */
function credit(v: unknown): number | null {
  if (typeof v === "number") return num(v);
  const o = obj(v);
  return num(o.usd) ?? num(o.remaining) ?? null;
}

/** Whether a paid call can go through: credit's {funded}, else a funded beside it; null when unknown. */
function funded(raw: unknown, beside: unknown): boolean | null {
  return flag(obj(raw).funded) ?? flag(beside);
}

/** The key's credit, as the page shows it. */
export interface Credit {
  /** US dollars, when OpenRouter gave a figure. */
  credit: number | null;
  /** Whether a paid call can go through; null when not known. */
  funded: boolean | null;
}

/**
 * No credit to spend, so the page offers Add credits: a figure of $0 or less, or, with no figure, OpenRouter saying
 * the account has none, which is how it answers an OAuth key on an account that never bought credits. Not known is
 * not empty.
 */
export function noCredit(c: Credit): boolean {
  return c.credit !== null ? c.credit <= 0 : c.funded === false;
}

// ---- the answers ---------------------------------------------------------------------------------------------

export interface OnboardState extends Credit {
  openrouter: boolean;
  /** The saved OpenRouter key was turned down (a 401): it must be signed in again, not topped up. */
  openrouterRejected: boolean;
  composio: boolean;
  composioSkipped: boolean;
  restarting: boolean;
  error: string;
}

export function readOnboard(raw: unknown): OnboardState {
  const o = obj(raw);
  return {
    openrouter: bool(o.openrouter),
    openrouterRejected: bool(o.openrouter_rejected),
    credit: credit(o.credit),
    funded: funded(o.credit, o.funded),
    composio: bool(o.composio),
    composioSkipped: bool(o.composio_skipped),
    restarting: bool(o.restarting),
    error: str(o.error, 300),
  };
}

/** Setup is over once a working model key is saved and Composio was signed in or skipped. A key that was turned
 * down (rejected) is not done: the Mac's setup page shows the re-sign-in there. */
export function setupDone(s: OnboardState): boolean {
  return s.openrouter && !s.openrouterRejected && (s.composio || s.composioSkipped);
}

export type LocalNetwork = "ok" | "blocked" | "unknown";

export interface WebStatus extends Credit {
  state: string;
  openrouter: boolean;
  /** The saved OpenRouter key was turned down (a 401): it must be signed in again, not topped up. */
  openrouterRejected: boolean;
  composio: boolean;
  voice: boolean;
  lan: boolean;
  localNetwork: LocalNetwork;
}

export function readStatus(raw: unknown): WebStatus {
  const o = obj(raw);
  const ln = o.local_network;
  return {
    state: str(o.state, 40),
    openrouter: bool(o.openrouter),
    openrouterRejected: bool(o.openrouter_rejected),
    composio: bool(o.composio),
    voice: bool(o.voice),
    lan: bool(o.lan),
    localNetwork: ln === "ok" || ln === "blocked" ? ln : "unknown",
    credit: credit(o.credit),
    funded: funded(o.credit, o.funded),
  };
}

export interface PairLink {
  label: string;
  url: string;
  /** A data:image/svg+xml (or png) URI the Mac drew; never a URL the page fetches. */
  qr: string;
}

export interface PairWindow {
  code: string;
  /** When the window closes, in ms since the epoch (this page's clock). */
  expiresAt: number;
  links: PairLink[];
}

const QR = /^data:image\/(svg\+xml|png)[;,]/;

function pairLink(label: string, url: unknown, qr: unknown): PairLink | null {
  if (typeof url !== "string" || !/^https:\/\//.test(url)) return null;
  return { label, url: url.slice(0, 500), qr: typeof qr === "string" && QR.test(qr) ? qr : "" };
}

export function readPairWindow(raw: unknown, now = Date.now()): PairWindow {
  const o = obj(raw);
  const links: PairLink[] = [];
  if (Array.isArray(o.links)) {
    for (const item of o.links) {
      const l = obj(item);
      const link = pairLink(str(l.label, 60) || "This device", l.url, l.qr);
      if (link) links.push(link);
    }
  } else {
    const apple = pairLink("iPhone or iPad", o.url_apple, o.qr_apple);
    const other = pairLink("Android or other", o.url_other, o.qr_other);
    if (apple) links.push(apple);
    if (other) links.push(other);
  }
  const expires = num(o.expires);
  const expiresIn = num(o.expires_in);
  const expiresAt =
    expires !== null ? expires * 1000 : expiresIn !== null ? now + expiresIn * 1000 : now + 5 * 60 * 1000;
  const code = typeof o.code === "string" && /^\d{6}$/.test(o.code) ? o.code : "";
  return { code, expiresAt, links };
}

export interface Device {
  id: string;
  label: string;
  host: string;
  created: number | null;
  lastSeen: number | null;
  current: boolean;
}

export function readDevices(raw: unknown): Device[] {
  const list = obj(raw).devices;
  if (!Array.isArray(list)) return [];
  const out: Device[] = [];
  for (const item of list) {
    const d = obj(item);
    const id = str(d.id, 80);
    if (!id) continue;
    out.push({
      id,
      label: str(d.label, 80) || "A device",
      host: str(d.host, 120),
      created: num(d.created),
      lastSeen: num(d.last_seen),
      current: bool(d.current),
    });
  }
  return out;
}

export interface AppEntry {
  toolkit: string;
  name: string;
  connected: boolean;
}

export interface AppsState {
  composio: boolean;
  apps: AppEntry[];
}

const TOOLKIT = /^[a-z0-9_-]{1,60}$/;

export function readApps(raw: unknown): AppsState {
  const o = obj(raw);
  const apps: AppEntry[] = [];
  if (Array.isArray(o.apps)) {
    for (const item of o.apps) {
      const a = obj(item);
      const toolkit = str(a.toolkit, 60).toLowerCase();
      if (!TOOLKIT.test(toolkit)) continue;
      apps.push({ toolkit, name: str(a.name, 60) || toolkit, connected: bool(a.connected) });
    }
  }
  return { composio: bool(o.composio), apps };
}

/** A Composio Connect Link: https on composio.dev or a subdomain, else null (the Mac checks this too). */
export function connectLink(raw: unknown): string | null {
  const url = str(obj(raw).url, 2000);
  try {
    const u = new URL(url);
    const host = u.hostname.toLowerCase();
    if (u.protocol !== "https:" || !(host === "composio.dev" || host.endsWith(".composio.dev"))) return null;
    return u.toString();
  } catch {
    return null;
  }
}

export interface RtcAnswer {
  sdp: string;
  voice: boolean;
}

export function readRtcAnswer(raw: unknown): RtcAnswer | null {
  const o = obj(raw);
  const sdp = o.sdp;
  if (typeof sdp !== "string" || !sdp.startsWith("v=0")) return null;
  return { sdp, voice: bool(o.voice) };
}
