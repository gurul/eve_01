// Fakes for the web client's tests (not a test file itself: vitest collects *.test.ts only).

import type { ChannelPort, Sink } from "../linkChannel";

export class FakePort implements ChannelPort {
  open = true;
  sent: string[] = [];
  private readonly messageListeners = new Set<(data: unknown) => void>();
  private readonly closeListeners = new Set<() => void>();

  isOpen() {
    return this.open;
  }
  send(text: string) {
    if (!this.open) throw new Error("closed");
    this.sent.push(text);
  }
  onMessage(listener: (data: unknown) => void) {
    this.messageListeners.add(listener);
    return () => this.messageListeners.delete(listener);
  }
  onClose(listener: () => void) {
    this.closeListeners.add(listener);
    return () => this.closeListeners.delete(listener);
  }
  deliver(data: unknown) {
    for (const l of [...this.messageListeners]) l(data);
  }
  close() {
    this.open = false;
    for (const l of [...this.closeListeners]) l();
  }
  frames(): Record<string, unknown>[] {
    return this.sent.map((s) => JSON.parse(s) as Record<string, unknown>);
  }
  lastReqId(): number {
    const reqs = this.frames().filter((f) => f.t === "req");
    return reqs.at(-1)!.id as number;
  }
}

export function recorder() {
  const got = { status: 0, headers: {} as Record<string, string>, body: [] as Uint8Array[], ended: false, error: null as Error | null };
  const sink: Sink = {
    head(s, h) {
      got.status = s;
      got.headers = h;
    },
    data(b) {
      got.body.push(b.slice());
    },
    end() {
      got.ended = true;
    },
    fail(e) {
      got.error = e;
    },
  };
  const text = () => new TextDecoder().decode(concat(got.body));
  return { got, sink, text };
}

function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/** A peer whose link opens, fails or dies when the test says. */
export class FakePeer {
  readonly port = new FakePort();
  readonly link: ChannelPort = this.port;
  readonly linkOpen: Promise<void>;
  readonly voice: boolean;
  readonly remote = {} as MediaStream;
  closed = false;
  mic: MediaStreamTrack | null = null;
  readonly calls: FakeCallPort[] = [];
  private resolveOpen: () => void = () => {};
  private rejectOpen: (e: Error) => void = () => {};
  private readonly downs = new Set<() => void>();

  constructor(voice = true) {
    this.voice = voice;
    this.port.open = false;
    this.linkOpen = new Promise<void>((resolve, reject) => {
      this.resolveOpen = resolve;
      this.rejectOpen = reject;
    });
    this.linkOpen.catch(() => {});
  }

  openLink(): void {
    this.port.open = true;
    this.resolveOpen();
  }

  die(): void {
    if (this.closed) return;
    this.closed = true;
    this.rejectOpen(new Error("down"));
    if (this.port.open) this.port.close();
    for (const d of [...this.downs]) d();
    this.downs.clear();
  }

  readonly setMic = async (track: MediaStreamTrack | null): Promise<void> => {
    this.mic = track;
  };

  channel(): RTCDataChannel {
    throw new Error("not in these tests");
  }

  openCall(): FakeCallPort {
    const port = new FakeCallPort();
    this.calls.push(port);
    return port;
  }

  alive(): boolean {
    return !this.closed && this.port.open;
  }

  onDown(listener: () => void): () => void {
    if (this.closed) {
      queueMicrotask(listener);
      return () => {};
    }
    this.downs.add(listener);
    return () => this.downs.delete(listener);
  }

  close(): void {
    this.die();
  }
}

/** The call channel's far end: what the page sent, and the Mac's messages on demand. */
export class FakeCallPort {
  open = false;
  closed = false;
  sent: Record<string, unknown>[] = [];
  private readonly openListeners: (() => void)[] = [];
  private readonly messageListeners: ((text: string) => void)[] = [];
  private readonly closeListeners: (() => void)[] = [];

  isOpen() {
    return this.open && !this.closed;
  }
  send(text: string) {
    if (!this.isOpen()) throw new Error("not open");
    this.sent.push(JSON.parse(text) as Record<string, unknown>);
  }
  close() {
    if (this.closed) return;
    this.closed = true;
    for (const l of this.closeListeners) l();
  }
  onOpen(l: () => void) {
    this.openListeners.push(l);
  }
  onMessage(l: (text: string) => void) {
    this.messageListeners.push(l);
  }
  onClose(l: () => void) {
    this.closeListeners.push(l);
  }
  /** The channel opens and the Mac says the call is up. */
  connect() {
    this.open = true;
    for (const l of this.openListeners) l();
    this.mac({ type: "state", state: "connected" });
  }
  mac(msg: Record<string, unknown>) {
    for (const l of this.messageListeners) l(JSON.stringify(msg));
  }
  types(): unknown[] {
    return this.sent.map((m) => m.type);
  }
}

/** A recorded fetch that answers each call with what the test queued, or holds it open. */
export function fakeFetch() {
  const calls: { url: string; init: RequestInit }[] = [];
  const answers: (() => Response | Promise<Response>)[] = [];
  const fetch = (url: string, init: RequestInit = {}) => {
    calls.push({ url, init });
    const answer = answers.shift();
    if (!answer) {
      // held open until aborted
      return new Promise<Response>((_, reject) => {
        init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
      });
    }
    return Promise.resolve(answer());
  };
  return { fetch, calls, answer: (fn: () => Response | Promise<Response>) => answers.push(fn) };
}
