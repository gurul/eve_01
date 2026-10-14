// Requests over the `link` DataChannel (framing.ts): one id each, at most MAX_IN_FLIGHT at once, a cancel that
// the Mac hears, and every open request failed with LinkLost when the channel goes or the Mac gives one up. The
// answer reaches a Sink as it arrives: the head, the body in pieces, then the end.

import {
  decodeServerFrame,
  encodeCancel,
  encodeReq,
  fitsChannel,
  MAX_IN_FLIGHT,
  nextId,
  type LinkMethod,
  type LinkPath,
} from "./framing";

/** One request to the /api/link door, as both the channel and the HTTPS fallback carry it. */
export interface LinkRequest {
  method: LinkMethod;
  path: LinkPath;
  /** The query string, without "?". */
  query: string;
  headers: Record<string, string>;
  /** "" for a GET. */
  body: string;
}

/** Where an answer goes. After end or fail, nothing more is called. */
export interface Sink {
  head(status: number, headers: Record<string, string>): void;
  data(bytes: Uint8Array): void;
  end(): void;
  fail(error: Error): void;
}

/** The request never got a whole answer because the direct link went (or the Mac gave it up): retryable. */
export class LinkLost extends Error {
  constructor(message = "The direct link to eve closed.") {
    super(message);
    this.name = "LinkLost";
  }
}

/** The part of an RTCDataChannel this needs (rtc.ts adapts the real one; tests give a fake). */
export interface ChannelPort {
  isOpen(): boolean;
  /** Throws when the channel cannot send. */
  send(text: string): void;
  onMessage(listener: (data: unknown) => void): () => void;
  onClose(listener: () => void): () => void;
}

/** Not a door path: the Mac answers it at once with head 404 (probe). */
export const PROBE_PATH = "/api/link/probe";

interface Pending {
  sink: Sink;
  headed: boolean;
}

export class LinkChannel {
  private readonly pending = new Map<number, Pending>();
  private lastId = 0;
  private closed = false;
  private readonly offs: (() => void)[];

  constructor(private readonly port: ChannelPort) {
    this.offs = [port.onMessage((data) => this.receive(data)), port.onClose(() => this.close())];
  }

  get open(): boolean {
    return !this.closed && this.port.isOpen();
  }

  get inFlight(): number {
    return this.pending.size;
  }

  /** Whether this request can go now: the channel is open, under its cap, and the body fits one frame. */
  takes(req: LinkRequest): boolean {
    return this.open && this.pending.size < MAX_IN_FLIGHT && fitsChannel(req.body);
  }

  /** Send it; returns the cancel. A request the channel cannot take fails at once with LinkLost. */
  request(req: LinkRequest, sink: Sink): () => void {
    if (!this.takes(req)) {
      queueMicrotask(() => sink.fail(new LinkLost("The direct link can't take that request.")));
      return () => {};
    }
    return this.send(req, sink);
  }

  /**
   * Whether the Mac still answers on this channel, within `ms`: a GET to PROBE_PATH, which it answers at once. A
   * page iOS froze can come back with a peer that still says "connected" while nothing flows; this finds that out
   * in seconds rather than at ICE's own timeout.
   */
  probe(ms: number): Promise<boolean> {
    if (!this.open) return Promise.resolve(false);
    if (this.pending.size >= MAX_IN_FLIGHT) return Promise.resolve(true); // busy is not dead
    return new Promise<boolean>((resolve) => {
      let settled = false;
      let cancel = () => {};
      const settle = (alive: boolean) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        cancel();
        resolve(alive);
      };
      const timer = setTimeout(() => settle(false), ms);
      const answered = () => settle(true);
      cancel = this.send(
        { method: "GET", path: PROBE_PATH, query: "", headers: {}, body: "" },
        // an `err` frame is an answer too; a channel that closed is not
        { head: answered, data: answered, end: answered, fail: () => settle(this.open) },
      );
    });
  }

  /** The channel went (or is being replaced): every open request fails with LinkLost. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    for (const off of this.offs) off();
    const open = [...this.pending.values()];
    this.pending.clear();
    for (const p of open) safely(() => p.sink.fail(new LinkLost()));
  }

  private send(req: { method: LinkMethod; path: string; query: string; headers: Record<string, string>; body: string }, sink: Sink): () => void {
    let id = nextId(this.lastId);
    while (this.pending.has(id)) id = nextId(id);
    this.lastId = id;
    this.pending.set(id, { sink, headed: false });
    try {
      this.port.send(encodeReq({ id, m: req.method, p: req.path, q: req.query, h: req.headers, b: req.body }));
    } catch {
      this.pending.delete(id);
      queueMicrotask(() => sink.fail(new LinkLost()));
      return () => {};
    }
    return () => this.cancel(id);
  }

  private cancel(id: number): void {
    if (!this.pending.delete(id) || !this.open) return;
    try {
      this.port.send(encodeCancel(id));
    } catch {
      // the channel is going; the Mac ends the request when it closes
    }
  }

  private finish(id: number, p: Pending, error?: Error): void {
    this.pending.delete(id);
    safely(() => (error ? p.sink.fail(error) : p.sink.end()));
  }

  private receive(data: unknown): void {
    const frame = decodeServerFrame(data);
    if (!frame) return;
    const p = this.pending.get(frame.id);
    if (!p) return; // cancelled, or never ours
    switch (frame.t) {
      case "head":
        if (p.headed) return this.finish(frame.id, p, new LinkLost("eve answered one request twice."));
        p.headed = true;
        if (!safely(() => p.sink.head(frame.status, frame.headers))) this.cancel(frame.id);
        return;
      case "data":
        if (!p.headed) return this.finish(frame.id, p, new LinkLost("eve's answer came without its head."));
        if (frame.bytes.length && !safely(() => p.sink.data(frame.bytes))) this.cancel(frame.id);
        return;
      case "end":
        return this.finish(frame.id, p, p.headed ? undefined : new LinkLost("eve's answer was empty."));
      case "err":
        return this.finish(frame.id, p, new LinkLost(frame.error ? `eve couldn't finish that: ${frame.error}` : undefined));
    }
  }
}

/** A sink that throws loses its own request (the Mac is told to stop it); the channel carries on. */
function safely(fn: () => void): boolean {
  try {
    fn();
    return true;
  } catch (e) {
    console.error("eve web: a link answer handler failed", e);
    return false;
  }
}
