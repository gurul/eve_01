// Push-to-talk in the browser (docs/plans/eve-lite.md, M6 "The call channel"; docs/voice-mode.md). The call is the
// phone call eve already has (phone_call.PhoneCalls), carried over WebRTC: the microphone rides the peer's audio
// transceiver, eve's voice comes back on the Mac's audio track, and a `call` DataChannel carries the call's JSON.
//
//   page -> Mac   {"type":"talk"} when the button goes down, {"type":"done"} when it comes up, {"type":"ping"}
//                 every 10 s (the Mac ends a call quiet for 30 s), {"type":"end"} on hang-up. The Mac builds the
//                 hello itself.
//   Mac -> page   {"type":"state","state":"connected|listening|thinking|speaking","note"?}, {"type":"heard","text"},
//                 {"type":"said","text"} (sent as that sentence's audio starts to leave the Mac, so the caption
//                 matches the voice), {"type":"flush"}, {"type":"ended","reason"}
//
// The microphone. Spike S5 (iOS audio output during push-to-talk) has not chosen between a hot microphone and
// capture per press, so this captures per press, the robust choice on iOS: the microphone is live only while the
// button is held, plus a short tail, and between presses the page is a plain playback page, so iOS plays eve's
// voice on the loudspeaker at full volume rather than as during a capture. The cost is the moment the microphone
// takes to start at each press; `talk` goes at once, so eve stops speaking at once. MIC_MODE "hot" keeps one
// capture for the whole call, the other S5 candidate.

export type MicMode = "per-press" | "hot";
export const MIC_MODE: MicMode = "per-press";
/** The page's keepalive (phone_call.QUIET_SECS is 30). */
export const PING_MS = 10_000;
/** After a release the microphone stays on this long, so its last audio still leaves (the Mac waits 250 ms). */
export const MIC_TAIL_MS = 350;
/** A call channel the Mac has not answered with "connected" by then is not coming (a peer that only looked alive). */
export const CONNECT_MS = 10_000;

export type VoicePhase = "off" | "connecting" | "ready" | "talking" | "thinking" | "speaking" | "ended" | "unavailable";

/** What the voice view shows as the way out of a problem. */
export type VoiceFix = "local-network" | "microphone" | "secure" | null;

export interface VoiceState {
  phase: VoicePhase;
  /** One line: what is happening, or what to do. */
  status: string;
  /** What eve heard, from the last press. */
  heard: string;
  /** The sentence eve is saying now. */
  said: string;
  fix: VoiceFix;
}

/** The call DataChannel as this needs it (callPortOf adapts the real one; tests give a fake). */
export interface CallPort {
  isOpen(): boolean;
  send(text: string): void;
  close(): void;
  onOpen(listener: () => void): void;
  onMessage(listener: (text: string) => void): void;
  onClose(listener: () => void): void;
}

/** What voice needs from the peer (rtc.RtcPeer). */
export interface VoicePeer {
  readonly voice: boolean;
  readonly remote: MediaStream;
  setMic(track: MediaStreamTrack | null): Promise<void>;
  openCall(): CallPort;
}

export interface VoiceDeps {
  /** A connected peer (Transport.peer). */
  peer(): Promise<VoicePeer>;
  /** The call is over (Transport.releaseVoice). */
  release(): void;
  /** One microphone track, asking for permission the first time. */
  mic(): Promise<MediaStreamTrack>;
  /** A secure page with a microphone API at all. */
  canCapture(): boolean;
}

export function callPortOf(dc: RTCDataChannel): CallPort {
  return {
    isOpen: () => dc.readyState === "open",
    send: (text) => dc.send(text),
    close: () => dc.close(),
    onOpen: (listener) => dc.addEventListener("open", () => listener()),
    onMessage: (listener) =>
      dc.addEventListener("message", (e: MessageEvent) => {
        if (typeof e.data === "string") listener(e.data);
      }),
    onClose: (listener) => dc.addEventListener("close", () => listener()),
  };
}

/** The browser's microphone: echo cancelled, as a call wants it. */
export async function browserMic(): Promise<MediaStreamTrack> {
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
  });
  const track = stream.getAudioTracks()[0];
  if (!track) throw new DOMException("no microphone", "NotFoundError");
  return track;
}

export const browserCanCapture = (): boolean =>
  typeof window !== "undefined" && window.isSecureContext && !!navigator.mediaDevices?.getUserMedia;

type CallEvent =
  | { type: "connected" }
  | { type: "state"; state: string; note: string }
  | { type: "heard"; text: string }
  | { type: "said"; text: string }
  | { type: "flush" }
  | { type: "ended"; reason: string }
  | { type: "closed" };

/** The call's JSON: sent once the Mac says connected (queued until then), with a ping every PING_MS. */
class CallLine {
  private connected = false;
  private queue: string[] = [];
  private ping: ReturnType<typeof setInterval> | undefined;
  private done = false;

  constructor(
    private readonly port: CallPort,
    private readonly on: (e: CallEvent) => void,
  ) {
    port.onOpen(() => this.startPing());
    if (port.isOpen()) this.startPing();
    port.onMessage((text) => this.receive(text));
    port.onClose(() => {
      if (this.done) return;
      this.done = true;
      clearInterval(this.ping);
      this.on({ type: "closed" });
    });
  }

  send(msg: { type: string }): void {
    const text = JSON.stringify(msg);
    if (this.connected) this.raw(text);
    else if (!this.done) this.queue.push(text);
  }

  /** Hang up: `end` if the call is up, then the channel closes. */
  close(): void {
    if (this.done) return;
    this.done = true;
    clearInterval(this.ping);
    if (this.connected) this.raw(JSON.stringify({ type: "end" }));
    this.queue = [];
    try {
      this.port.close();
    } catch {
      // already closed
    }
  }

  private raw(text: string): void {
    try {
      if (this.port.isOpen()) this.port.send(text);
    } catch {
      // closing; onClose reports it
    }
  }

  private startPing(): void {
    if (this.ping !== undefined || this.done) return;
    this.ping = setInterval(() => this.raw(JSON.stringify({ type: "ping" })), PING_MS);
  }

  private receive(text: string): void {
    let msg: Record<string, unknown>;
    try {
      const parsed: unknown = JSON.parse(text);
      if (!parsed || typeof parsed !== "object") return;
      msg = parsed as Record<string, unknown>;
    } catch {
      return;
    }
    const str = (v: unknown) => (typeof v === "string" ? v.slice(0, 2000) : "");
    switch (msg.type) {
      case "state": {
        const state = str(msg.state);
        if (state === "connected") {
          if (this.connected) return;
          this.connected = true;
          const queued = this.queue;
          this.queue = [];
          for (const q of queued) this.raw(q);
          this.on({ type: "connected" });
        } else {
          this.on({ type: "state", state, note: str(msg.note) });
        }
        return;
      }
      case "heard":
        return this.on({ type: "heard", text: str(msg.text) });
      case "said":
        return this.on({ type: "said", text: str(msg.text) });
      case "flush":
        return this.on({ type: "flush" });
      case "ended":
        this.done = true;
        clearInterval(this.ping);
        this.on({ type: "ended", reason: str(msg.reason) });
        try {
          this.port.close();
        } catch {
          // already closed
        }
        return;
      default:
        return; // pong, expression: nothing to show here
    }
  }
}

export const VOICE_TEXT = {
  connecting: "Connecting…",
  ready: "Hold to talk",
  listening: "Listening…",
  thinking: "eve is thinking…",
  speaking: "eve is speaking",
  dropped: "The call dropped.",
  ended: "The call ended.",
  secure: "Voice needs eve's secure address.",
  noMic: "There's no microphone on this device.",
  micBlocked: "eve can't hear you: the microphone is blocked for this site.",
  noLink: "Voice needs a direct link to the Mac.",
  noVoice: "Voice isn't set up on this Mac.",
} as const;

/** The Mac's reason as a line: its own sentences as they are ("Only eve's owner can call."), its short reasons
 * ("the phone went quiet") after "The call ended:". */
export function endedLine(reason: string): string {
  const r = reason.trim().replace(/\.$/, "");
  if (!r) return VOICE_TEXT.ended;
  return /^[a-z]/.test(r) ? `The call ended: ${r}.` : `${r}.`;
}

function micProblem(e: unknown): { status: string; fix: VoiceFix } {
  const name = e instanceof DOMException || e instanceof Error ? e.name : "";
  if (name === "NotFoundError" || name === "OverconstrainedError") return { status: VOICE_TEXT.noMic, fix: null };
  return { status: VOICE_TEXT.micBlocked, fix: "microphone" };
}

export class VoiceController {
  private state: VoiceState = { phase: "off", status: "", heard: "", said: "", fix: null };
  private readonly listeners = new Set<() => void>();
  private session = 0;
  private peer: VoicePeer | null = null;
  private line: CallLine | null = null;
  private mic: MediaStreamTrack | null = null;
  private micAsk: Promise<void> | null = null;
  private pressed = false;
  private tail: ReturnType<typeof setTimeout> | undefined;
  private connectTimer: ReturnType<typeof setTimeout> | undefined;
  private audio: HTMLAudioElement | null = null;

  constructor(private readonly deps: VoiceDeps) {}

  /** For useSyncExternalStore: stable functions, the same state object until it changes. */
  readonly getState = (): VoiceState => this.state;

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /** The element eve's voice plays in (`playsinline`). */
  attachAudio(el: HTMLAudioElement | null): void {
    this.audio = el;
    if (el && this.peer && el.srcObject !== this.peer.remote) el.srcObject = this.peer.remote;
  }

  /** Open the call: the microphone's permission first, then the peer, then the call channel. */
  async start(): Promise<void> {
    const session = ++this.session;
    this.closeCall(true);
    if (!this.deps.canCapture()) {
      this.set({ phase: "unavailable", status: VOICE_TEXT.secure, fix: "secure", heard: "", said: "" });
      return;
    }
    this.set({ phase: "connecting", status: VOICE_TEXT.connecting, fix: null, heard: "", said: "" });
    try {
      const track = await this.deps.mic(); // asks for permission now, not at the first press
      if (session !== this.session || MIC_MODE === "per-press") track.stop();
      else this.mic = track;
    } catch (e) {
      if (session === this.session) this.set({ phase: "unavailable", ...micProblem(e) });
      return;
    }
    if (session !== this.session) return;
    let peer: VoicePeer;
    try {
      peer = await this.deps.peer();
    } catch (e) {
      if (session !== this.session) return;
      // a browser with no WebRTC at all (rtc.NoWebRtc) says so; anything else is the Mac's link being blocked
      if (e instanceof Error && e.name === "NoWebRtc") this.set({ phase: "unavailable", status: e.message, fix: null });
      else this.set({ phase: "unavailable", status: VOICE_TEXT.noLink, fix: "local-network" });
      return;
    }
    if (session !== this.session) return;
    if (!peer.voice) {
      this.set({ phase: "unavailable", status: VOICE_TEXT.noVoice, fix: null });
      return;
    }
    this.peer = peer;
    if (this.audio) this.audio.srcObject = peer.remote;
    if (this.mic) void peer.setMic(this.mic).catch(() => {});
    let port: CallPort;
    try {
      port = peer.openCall();
    } catch {
      this.set({ phase: "unavailable", status: VOICE_TEXT.noLink, fix: "local-network" });
      return;
    }
    this.line = new CallLine(port, (e) => {
      if (session === this.session) this.event(e);
    });
    this.connectTimer = setTimeout(() => {
      if (session !== this.session || this.state.phase !== "connecting") return;
      this.closeCall(true);
      this.set({ phase: "unavailable", status: VOICE_TEXT.noLink, fix: "local-network" });
    }, CONNECT_MS);
  }

  /** The button went down: eve stops talking and listens. */
  press(): void {
    const phase = this.state.phase;
    if (this.pressed || !this.line || !["ready", "thinking", "speaking", "talking"].includes(phase)) return;
    this.pressed = true;
    clearTimeout(this.tail);
    this.unlockAudio();
    this.line.send({ type: "talk" });
    this.set({ phase: "talking", status: VOICE_TEXT.listening, said: "", heard: "", fix: null });
    void this.micOn();
  }

  /** The button came up: what was said goes to eve. */
  release(): void {
    if (!this.pressed) return;
    this.pressed = false;
    this.line?.send({ type: "done" });
    if (this.state.phase === "talking") this.set({ phase: "thinking", status: VOICE_TEXT.thinking });
    if (MIC_MODE === "per-press") {
      clearTimeout(this.tail);
      this.tail = setTimeout(() => this.micOff(), MIC_TAIL_MS);
    }
  }

  /** Hang up and let go of the microphone and the peer. */
  stop(): void {
    this.session += 1;
    this.closeCall(true);
    this.set({ phase: "off", status: "", heard: "", said: "", fix: null });
  }

  private closeCall(hangUp: boolean): void {
    clearTimeout(this.connectTimer);
    if (this.pressed) {
      this.pressed = false;
      this.line?.send({ type: "done" });
    }
    clearTimeout(this.tail);
    if (hangUp) this.line?.close();
    this.line = null;
    this.micOff();
    if (this.audio) {
      this.audio.pause();
      this.audio.srcObject = null;
    }
    if (this.peer) {
      this.peer = null;
      this.deps.release();
    }
  }

  private unlockAudio(): void {
    const el = this.audio;
    if (!el) return;
    if (this.peer && el.srcObject !== this.peer.remote) el.srcObject = this.peer.remote;
    void el.play().catch(() => {});
  }

  private async micOn(): Promise<void> {
    const peer = this.peer;
    if (!peer) return;
    if (this.mic && this.mic.readyState === "live") {
      await peer.setMic(this.mic).catch(() => {});
      return;
    }
    if (this.micAsk) return this.micAsk;
    this.micAsk = (async () => {
      try {
        const track = await this.deps.mic();
        // The call ended, or the button came up before the microphone was ready (a tap): nothing to send.
        if (this.peer !== peer || (!this.pressed && MIC_MODE === "per-press")) {
          track.stop();
          return;
        }
        this.mic = track;
        await peer.setMic(track).catch(() => {});
      } catch (e) {
        if (this.peer !== peer) return;
        if (this.pressed) this.release();
        this.set({ phase: "ready", ...micProblem(e) });
      } finally {
        this.micAsk = null;
      }
    })();
    return this.micAsk;
  }

  private micOff(): void {
    clearTimeout(this.tail);
    this.tail = undefined;
    if (this.pressed && this.line) return;
    const track = this.mic;
    if (MIC_MODE === "hot" && this.line && track) return;
    this.mic = null;
    if (this.peer) void this.peer.setMic(null).catch(() => {});
    track?.stop();
  }

  private event(e: CallEvent): void {
    switch (e.type) {
      case "connected":
        clearTimeout(this.connectTimer);
        this.set({ phase: this.pressed ? "talking" : "ready", status: this.pressed ? VOICE_TEXT.listening : VOICE_TEXT.ready });
        return;
      case "state":
        if (this.pressed) return; // the button is down: "Listening…" stands
        if (e.state === "thinking") this.set({ phase: "thinking", status: e.note || VOICE_TEXT.thinking });
        else if (e.state === "speaking") this.set({ phase: "speaking", status: VOICE_TEXT.speaking });
        else if (e.state === "listening") this.set({ phase: "ready", status: e.note || VOICE_TEXT.ready });
        return;
      case "heard":
        this.set({ heard: e.text });
        return;
      case "said":
        if (!this.pressed) this.set({ said: e.text });
        return;
      case "flush":
        this.set({ said: "" });
        return;
      case "ended":
      case "closed": {
        const status = e.type === "closed" ? VOICE_TEXT.dropped : endedLine(e.reason);
        this.line = null;
        this.closeCall(false);
        this.set({ phase: "ended", status, said: "" });
        return;
      }
    }
  }

  private set(patch: Partial<VoiceState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of [...this.listeners]) listener();
  }
}
