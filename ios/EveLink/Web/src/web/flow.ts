// What the page shows, decided at load and whenever the sign-in changes (Root.tsx draws it):
//
//   the Mac's own browser   signed in (eve_mac)? setup done (OpenRouter, then Composio or Skip)? then the chat
//   a phone or laptop       paired (eve_dev)? then the chat
//
// A one-time secret from the fragment (pair.ts) is spent first. Each sign-in writes a credential and eve restarts,
// so a page that comes back to a restarting eve waits for it and carries on by itself. eve not answering at all
// (asleep, restarting, another network) is retried, more slowly each time.

import { api, ApiError, readOnboard, readStatus, setupDone, UNREACHABLE, type Fetch, type OnboardState, type Where } from "./api";
import { DEVICE_UNPAIRED, MAC_SIGNED_OUT } from "./host";
import { macLogin, pairDevice, type Fragment } from "./pair";

export type Phase =
  | { name: "boot" }
  | { name: "signin"; note: string }
  | { name: "pair"; note: string }
  | { name: "setup"; state: OnboardState }
  | { name: "restarting"; late: boolean }
  | { name: "unreachable"; message: string }
  | { name: "chat" };

/** Paired, yet the next request carried no cookie: this browser did not keep it (a certificate it distrusts). */
export const COOKIE_NOT_KEPT =
  "This browser paired but didn't keep eve's sign-in. Trust eve's certificate on this device (below), then scan the code again.";
export const NOT_SET_UP = "eve isn't set up yet. Finish setting it up on the Mac, then try again.";
export const RESTART_POLL_MS = 1000;
export const RESTART_LATE_MS = 30_000;
const RETRY_FIRST_MS = 2000;
const RETRY_MAX_MS = 15_000;

/** What spending the fragment's secret left: a line to show, and whether a pairing just succeeded. */
export interface Spent {
  note: string;
  justPaired: boolean;
}

/** How often a secret is posted again while eve does not answer at all (it may be starting). */
const SPEND_TRIES = 3;
const SPEND_WAIT_MS = 1500;

/**
 * Post the fragment's secret once (a login code on the Mac, a pairing secret on a phone). Only "no answer at all"
 * is tried again, a little later: an answer, even a refusal, means the secret was seen, and it is single use.
 */
export async function spendSecret(
  secret: Fragment | null,
  fetchFn?: Fetch,
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<Spent> {
  if (!secret) return { note: "", justPaired: false };
  for (let attempt = 1; ; attempt++) {
    const outcome =
      secret.kind === "login" ? await macLogin(secret.code, fetchFn) : await pairDevice({ secret: secret.secret }, fetchFn);
    if (outcome.ok) return { note: "", justPaired: secret.kind === "pair" };
    if (outcome.status !== 0 || attempt >= SPEND_TRIES) return { note: outcome.message, justPaired: false };
    await sleep(SPEND_WAIT_MS * attempt);
  }
}

export interface FlowOptions {
  where: Where;
  onPhase(phase: Phase): void;
  fetch?: Fetch;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

export class Flow {
  /** Bumped by anything that makes an answer in flight stale. */
  private gen = 0;
  private retryTimer: ReturnType<typeof setTimeout> | undefined;
  private retryWait = RETRY_FIRST_MS;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly now: () => number;

  constructor(private readonly opts: FlowOptions) {
    this.sleep = opts.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
    this.now = opts.now ?? Date.now;
  }

  /** Load: the spent secret's outcome, then the decision. */
  async boot(spent: Promise<Spent>): Promise<void> {
    const gen = ++this.gen;
    const { note, justPaired } = await spent;
    if (gen === this.gen) await this.decide(note, justPaired);
  }

  async decide(note = "", justPaired = false): Promise<void> {
    const gen = ++this.gen;
    clearTimeout(this.retryTimer);
    const { where } = this.opts;
    try {
      if (where === "mac") {
        const state = readOnboard(await api("GET", "/onboard/state", undefined, this.opts.fetch));
        if (gen !== this.gen) return;
        this.retryWait = RETRY_FIRST_MS;
        if (state.restarting) return await this.waitForRestart(gen);
        this.show(setupDone(state) ? { name: "chat" } : { name: "setup", state });
        return;
      }
      const status = readStatus(await api("GET", "/api/web/status", undefined, this.opts.fetch));
      if (gen !== this.gen) return;
      this.retryWait = RETRY_FIRST_MS;
      this.show(/needs.?openrouter/i.test(status.state) ? { name: "unreachable", message: NOT_SET_UP } : { name: "chat" });
    } catch (e) {
      if (gen !== this.gen) return;
      if (e instanceof ApiError && e.status === 401) {
        this.show(where === "mac" ? { name: "signin", note } : { name: "pair", note: justPaired ? COOKIE_NOT_KEPT : note });
        return;
      }
      this.show({ name: "unreachable", message: e instanceof ApiError ? e.message : UNREACHABLE });
      const wait = this.retryWait;
      this.retryWait = Math.min(wait * 2, RETRY_MAX_MS);
      this.retryTimer = setTimeout(() => void this.decide(note, justPaired), wait);
    }
  }

  /** A request answered 401 in the chat: the cookie is gone or was revoked. */
  authLost(): void {
    this.gen += 1;
    clearTimeout(this.retryTimer);
    this.show(this.opts.where === "mac" ? { name: "signin", note: MAC_SIGNED_OUT } : { name: "pair", note: DEVICE_UNPAIRED });
  }

  /** The owner signed this browser out. */
  signedOut(): void {
    this.gen += 1;
    clearTimeout(this.retryTimer);
    this.show(this.opts.where === "mac" ? { name: "signin", note: "" } : { name: "pair", note: "" });
  }

  dispose(): void {
    this.gen += 1;
    clearTimeout(this.retryTimer);
  }

  private show(phase: Phase): void {
    this.opts.onPhase(phase);
  }

  /** eve said it is restarting: wait until a new eve answers that it is not, then decide again. */
  private async waitForRestart(gen: number): Promise<void> {
    this.show({ name: "restarting", late: false });
    const started = this.now();
    let late = false;
    while (gen === this.gen) {
      await this.sleep(RESTART_POLL_MS);
      if (gen !== this.gen) return;
      try {
        const state = readOnboard(await api("GET", "/onboard/state", undefined, this.opts.fetch));
        if (gen !== this.gen) return;
        if (!state.restarting) return void this.decide();
      } catch (e) {
        if (gen !== this.gen) return;
        if (e instanceof ApiError && e.status !== 0) return void this.decide();
        // not answering while it restarts: keep waiting
      }
      if (!late && this.now() - started > RESTART_LATE_MS) {
        late = true;
        this.show({ name: "restarting", late: true });
      }
    }
  }
}
