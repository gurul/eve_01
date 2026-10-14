// Which way a chat request goes (docs/plans/eve-lite.md, M6 "Fallback"):
//
// * The Mac's own page (http://localhost) always fetches over loopback. Accepting TCP needs no Local Network
//   permission, so nothing on the Mac can block it.
// * A phone or laptop asks over the `link` DataChannel. At first it waits up to FIRST_WAIT_MS for the channel to
//   open; past that, chat goes over HTTPS to the same door, and the status line says the direct link is blocked.
//   WebRTC is retried with backoff from RETRY_MIN_MS to RETRY_MAX_MS, and new requests return to the DataChannel
//   once it opens. A request already on HTTPS finishes there.
// * A request the channel cannot take (MAX_IN_FLIGHT already open, or a body over MAX_REQ_BODY) goes over HTTPS.
//
// Voice uses the same peer on a phone (its audio transceiver and a `call` channel). The Mac's own page connects a
// peer only for voice, and lets it go when the call ends.

import { ApiError, UNREACHABLE, type Fetch } from "./api";
import { LinkChannel, type LinkRequest, type Sink } from "./linkChannel";
import { NoWebRtc, type RtcPeer } from "./rtc";

export type LinkStatus = "connecting" | "direct" | "fallback" | "loopback";

/** How long a page that just opened waits for the DataChannel before chat goes over HTTPS. */
export const FIRST_WAIT_MS = 4000;
/** An attempt that has not opened the channel by then is given up. */
export const ATTEMPT_MS = 15_000;
export const RETRY_MIN_MS = 5000;
export const RETRY_MAX_MS = 60_000;
/** On waking, a link that does not answer a probe this soon is taken as dead. */
export const PROBE_MS = 2500;
/** The /api/link door over HTTPS (lite_web.py passes these to LinkChat.serve). */
export const HTTPS_BASE = "/api/web/link";

/** fetch failed outright (no answer at all). Not retried: the poll and the owner retry. */
export class Unreachable extends Error {
  constructor(message = UNREACHABLE) {
    super(message);
    this.name = "Unreachable";
  }
}

export interface TransportOptions {
  /** The Mac's own page: chat over loopback fetch, a peer only for voice. */
  loopback: boolean;
  /** Offer, signal and answer (rtc.connectPeer). */
  connect: (signal: AbortSignal) => Promise<RtcPeer>;
  fetch: Fetch;
  /** Signaling answered 401: this device is not signed in any more. */
  onUnauthorized?: () => void;
  /**
   * Whether the page is in view. A hidden page makes no new offer: the Mac keeps one peer per device and the
   * newest offer wins, so two tabs retrying in turn would close each other's link for ever. wake() resumes.
   */
  isVisible?: () => boolean;
}

export interface RequestOptions {
  /** "fetch": HTTPS (or loopback) whatever the channel's state, for a retry after the link dropped. */
  via?: "fetch";
}

export type Via = "channel" | "fetch";

export interface Handle {
  cancel(): void;
  /** Where it went, once routed. */
  readonly via: Promise<Via>;
}

function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(new DOMException("aborted", "AbortError"));
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(new DOMException("aborted", "AbortError"));
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(
      (v) => {
        signal.removeEventListener("abort", onAbort);
        resolve(v);
      },
      (e) => {
        signal.removeEventListener("abort", onAbort);
        reject(e);
      },
    );
  });
}

function headersOf(h: Headers): Record<string, string> {
  const out: Record<string, string> = {};
  h.forEach((value, key) => {
    out[key.toLowerCase()] = value;
  });
  return out;
}

export class Transport {
  private _status: LinkStatus;
  private readonly statusListeners = new Set<(s: LinkStatus) => void>();
  private chat: { peer: RtcPeer; channel: LinkChannel } | null = null;
  private attempting: Promise<RtcPeer> | null = null;
  private attemptAbort: AbortController | null = null;
  private retryTimer: ReturnType<typeof setTimeout> | undefined;
  private firstTimer: ReturnType<typeof setTimeout> | undefined;
  private backoff = RETRY_MIN_MS;
  /** Requests held while the first attempt may still open the channel. */
  private waiting: (() => void)[] = [];
  private firstWindow = false;
  private running = false;
  /** Bumped by stop(): an attempt from before it never adopts, fails or retries anything. */
  private generation = 0;
  private voicePeer: RtcPeer | null = null;
  private voiceConnecting: Promise<RtcPeer> | null = null;

  constructor(private readonly opts: TransportOptions) {
    this._status = opts.loopback ? "loopback" : "connecting";
  }

  get status(): LinkStatus {
    return this._status;
  }

  onStatus(listener: (s: LinkStatus) => void): () => void {
    this.statusListeners.add(listener);
    return () => this.statusListeners.delete(listener);
  }

  /** Begin connecting (a phone or laptop); the Mac's own page has nothing to start. */
  start(): void {
    if (this.running || this.opts.loopback) return;
    this.running = true;
    this.backoff = RETRY_MIN_MS;
    this.firstWindow = true;
    this.setStatus("connecting");
    this.firstTimer = setTimeout(() => this.endFirstWindow(), FIRST_WAIT_MS);
    void this.attempt().catch(() => {});
  }

  /** Close everything; requests already sent end with the peer. */
  stop(): void {
    this.running = false;
    this.generation += 1;
    clearTimeout(this.retryTimer);
    clearTimeout(this.firstTimer);
    this.attemptAbort?.abort();
    this.attemptAbort = null;
    this.attempting = null;
    this.dropChat();
    this.endFirstWindow();
    this.voicePeer?.close();
    this.voicePeer = null;
    this.voiceConnecting = null;
  }

  /** The page is visible again (iOS may have frozen it): check the link, and retry at once if it is down. */
  wake(): void {
    if (!this.running) return;
    const chat = this.chat;
    if (chat && chat.peer.alive()) {
      void chat.channel.probe(PROBE_MS).then((alive) => {
        if (alive || this.chat !== chat) return;
        this.lost(chat.peer);
        this.retryNow();
      });
      return;
    }
    if (chat) this.lost(chat.peer);
    this.retryNow();
  }

  /** Send one request to the /api/link door the best way there is now. */
  request(req: LinkRequest, sink: Sink, options: RequestOptions = {}): Handle {
    let cancelled = false;
    let cancelInner: (() => void) | null = null;
    let routed!: (via: Via) => void;
    const via = new Promise<Via>((resolve) => {
      routed = resolve;
    });
    const go = (way: Via) => {
      if (cancelled) return;
      const chat = this.chat;
      if (way === "channel" && chat && chat.channel.takes(req)) {
        routed("channel");
        cancelInner = chat.channel.request(req, sink);
      } else {
        routed("fetch");
        cancelInner = this.fetchRequest(req, sink);
      }
    };
    if (options.via === "fetch" || this.opts.loopback) go("fetch");
    else if (this.chat?.channel.takes(req)) go("channel");
    else if (this.firstWindow) this.waiting.push(() => go("channel"));
    else go("fetch");
    return {
      cancel: () => {
        cancelled = true;
        cancelInner?.();
      },
      via,
    };
  }

  /** A connected peer for voice: the chat peer on a phone (connecting now if it is down), a voice-only one on the Mac. */
  async peer(): Promise<RtcPeer> {
    if (this.opts.loopback) return this.loopbackPeer();
    if (!this.running) this.start();
    if (this.chat?.peer.alive()) return this.chat.peer;
    if (this.chat) this.lost(this.chat.peer);
    return this.attempting ?? this.attempt();
  }

  /** The call ended: the Mac's own page closes its voice-only peer. */
  releaseVoice(): void {
    if (!this.opts.loopback) return;
    this.voicePeer?.close();
    this.voicePeer = null;
  }

  // ---- WebRTC attempts --------------------------------------------------------------------------------------

  private setStatus(s: LinkStatus): void {
    if (this._status === s) return;
    this._status = s;
    for (const listener of [...this.statusListeners]) listener(s);
  }

  private endFirstWindow(): void {
    clearTimeout(this.firstTimer);
    if (!this.firstWindow) return;
    this.firstWindow = false;
    if (!this.chat && this.running) this.setStatus("fallback");
    const held = this.waiting;
    this.waiting = [];
    for (const go of held) go();
  }

  private attempt(): Promise<RtcPeer> {
    if (this.attempting) return this.attempting;
    clearTimeout(this.retryTimer);
    const generation = this.generation;
    const current = () => generation === this.generation;
    const abort = new AbortController();
    this.attemptAbort = abort;
    const timer = setTimeout(() => abort.abort(), ATTEMPT_MS);
    const run = (async () => {
      let peer: RtcPeer | null = null;
      try {
        peer = await this.opts.connect(abort.signal);
        await abortable(peer.linkOpen, abort.signal);
        if (!this.running || !current()) throw new DOMException("stopped", "AbortError");
        this.adopt(peer);
        return peer;
      } catch (e) {
        peer?.close();
        if (current()) this.failed(e);
        throw e;
      } finally {
        clearTimeout(timer);
        if (current()) {
          if (this.attemptAbort === abort) this.attemptAbort = null;
          this.attempting = null;
        }
      }
    })();
    this.attempting = run;
    return run;
  }

  private adopt(peer: RtcPeer): void {
    this.dropChat();
    this.chat = { peer, channel: new LinkChannel(peer.link) };
    this.backoff = RETRY_MIN_MS;
    this.setStatus("direct");
    peer.onDown(() => this.lost(peer));
    this.endFirstWindow();
  }

  private failed(e: unknown): void {
    if (e instanceof NoWebRtc) {
      // nothing to retry: HTTPS for good
      this.endFirstWindow();
      this.setStatus("fallback");
      return;
    }
    if (e instanceof ApiError && e.status === 401) {
      this.running = false;
      this.endFirstWindow();
      this.setStatus("fallback");
      this.opts.onUnauthorized?.();
      return;
    }
    if (!this.running) return;
    this.endFirstWindow();
    this.setStatus("fallback");
    this.scheduleRetry();
  }

  /** The direct peer died: its open requests fail (LinkLost), new ones go over HTTPS, and WebRTC is retried. */
  private lost(peer: RtcPeer): void {
    if (this.chat?.peer !== peer) return;
    this.dropChat();
    if (!this.running) return;
    this.setStatus("fallback");
    this.scheduleRetry();
  }

  private dropChat(): void {
    const chat = this.chat;
    this.chat = null;
    if (!chat) return;
    chat.channel.close();
    chat.peer.close();
  }

  private retryNow(): void {
    if (!this.running || this.chat || this.attempting) return;
    this.backoff = RETRY_MIN_MS;
    void this.attempt().catch(() => {});
  }

  private scheduleRetry(): void {
    clearTimeout(this.retryTimer);
    const wait = this.backoff;
    this.backoff = Math.min(this.backoff * 2, RETRY_MAX_MS);
    this.retryTimer = setTimeout(() => {
      if (!this.running || this.chat) return;
      if (this.opts.isVisible && !this.opts.isVisible()) return; // wake() tries again when the page shows
      void this.attempt().catch(() => {});
    }, wait);
  }

  private async loopbackPeer(): Promise<RtcPeer> {
    if (this.voicePeer?.alive()) return this.voicePeer;
    this.voicePeer?.close();
    this.voicePeer = null;
    if (this.voiceConnecting) return this.voiceConnecting;
    const generation = this.generation;
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), ATTEMPT_MS);
    const run = (async () => {
      let peer: RtcPeer | null = null;
      try {
        peer = await this.opts.connect(abort.signal);
        await abortable(peer.linkOpen, abort.signal);
        if (generation !== this.generation) throw new DOMException("stopped", "AbortError");
        this.voicePeer = peer;
        const mine = peer;
        peer.onDown(() => {
          if (this.voicePeer === mine) this.voicePeer = null;
        });
        return peer;
      } catch (e) {
        peer?.close();
        throw e;
      } finally {
        clearTimeout(timer);
        if (generation === this.generation) this.voiceConnecting = null;
      }
    })();
    this.voiceConnecting = run;
    return run;
  }

  // ---- HTTPS (or loopback) ------------------------------------------------------------------------------------

  private fetchRequest(req: LinkRequest, sink: Sink): () => void {
    const abort = new AbortController();
    const url = HTTPS_BASE + req.path.slice("/api/link".length) + (req.query ? `?${req.query}` : "");
    void (async () => {
      let res: Response;
      try {
        res = await this.opts.fetch(url, {
          method: req.method,
          headers: req.headers,
          body: req.method === "GET" ? undefined : req.body,
          credentials: "same-origin",
          cache: "no-store",
          signal: abort.signal,
        });
      } catch {
        if (!abort.signal.aborted) sink.fail(new Unreachable());
        return;
      }
      if (abort.signal.aborted) return;
      sink.head(res.status, headersOf(res.headers));
      const reader = res.body?.getReader();
      if (!reader) {
        sink.end();
        return;
      }
      abort.signal.addEventListener("abort", () => void reader.cancel().catch(() => {}), { once: true });
      try {
        for (;;) {
          const { value, done } = await reader.read();
          if (abort.signal.aborted) return;
          if (done) break;
          if (value && value.length) sink.data(value);
        }
      } catch {
        if (!abort.signal.aborted) sink.fail(new Unreachable("The connection to eve dropped."));
        return;
      }
      sink.end();
    })();
    return () => abort.abort();
  }
}
