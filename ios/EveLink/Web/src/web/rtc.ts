// The WebRTC peer to the Mac (docs/plans/eve-lite.md, M6). No STUN or TURN server (iceServers: []): eve is on the
// same Wi-Fi, and nothing leaves the network. Every candidate goes in the offer (no trickle: the Mac's answer
// carries all of its own). The `link` DataChannel is made before the offer, with one sendrecv audio transceiver
// and no track, so voice later attaches the microphone with replaceTrack and nothing is renegotiated; the `call`
// channel opens on demand on the same association.

import { api, readRtcAnswer, type Fetch } from "./api";
import type { ChannelPort } from "./linkChannel";

/** The Mac refuses a longer offer. */
export const MAX_OFFER_CHARS = 20_000;
/** Host candidates only, so gathering takes milliseconds; past this the offer goes with what it has. */
const GATHER_MS = 2500;
/** "disconnected" often heals by itself; this long without healing counts as down. */
const DISCONNECTED_MS = 6000;

export interface RtcPeer {
  /** The link channel, for linkChannel.LinkChannel. */
  readonly link: ChannelPort;
  /** Resolves when the link channel opens; rejects when the peer dies first. */
  readonly linkOpen: Promise<void>;
  /** The Mac can take a call (its answer's "voice"). */
  readonly voice: boolean;
  /** eve's voice: the Mac's audio track. */
  readonly remote: MediaStream;
  /** Send this microphone track, or nothing. */
  setMic(track: MediaStreamTrack | null): Promise<void>;
  /** Another DataChannel on the same association (voice's "call"). */
  channel(label: string): RTCDataChannel;
  /** Whether the connection still looks usable. */
  alive(): boolean;
  /** Called once, when the peer dies: failed, closed, or its link channel closed. */
  onDown(listener: () => void): () => void;
  close(): void;
}

/** A real RTCDataChannel as linkChannel's port: binary frames arrive as ArrayBuffers. */
export function portOf(dc: RTCDataChannel): ChannelPort {
  dc.binaryType = "arraybuffer";
  return {
    isOpen: () => dc.readyState === "open",
    send: (text) => dc.send(text),
    onMessage(listener) {
      const handler = (e: MessageEvent) => listener(e.data);
      dc.addEventListener("message", handler);
      return () => dc.removeEventListener("message", handler);
    },
    onClose(listener) {
      const handler = () => listener();
      dc.addEventListener("close", handler);
      return () => dc.removeEventListener("close", handler);
    },
  };
}

function gathered(pc: RTCPeerConnection, ms: number): Promise<void> {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      pc.removeEventListener("icegatheringstatechange", onState);
      pc.removeEventListener("icecandidate", onCandidate);
      resolve();
    };
    const onState = () => {
      if (pc.iceGatheringState === "complete") done();
    };
    const onCandidate = (e: RTCPeerConnectionIceEvent) => {
      if (!e.candidate) done();
    };
    const timer = setTimeout(done, ms);
    pc.addEventListener("icegatheringstatechange", onState);
    pc.addEventListener("icecandidate", onCandidate);
  });
}

export interface ConnectOptions {
  fetch?: Fetch;
  signal?: AbortSignal;
}

/** This browser has no WebRTC (an old one, or turned off, as iOS Lockdown Mode does): chat stays on HTTPS. */
export class NoWebRtc extends Error {
  constructor() {
    super("This browser has no WebRTC, so eve chats over HTTPS and voice is off.");
    this.name = "NoWebRtc";
  }
}

export function webRtcAvailable(): boolean {
  return typeof RTCPeerConnection === "function" && typeof MediaStream === "function";
}

/** Offer, POST /api/web/rtc, answer. Resolves once the answer is set; `linkOpen` says when chat can use it. */
export async function connectPeer(opts: ConnectOptions = {}): Promise<RtcPeer> {
  if (!webRtcAvailable()) throw new NoWebRtc();
  const pc = new RTCPeerConnection({ iceServers: [] });
  const dc = pc.createDataChannel("link", { ordered: true });
  const link = portOf(dc);
  const audio = pc.addTransceiver("audio", { direction: "sendrecv" });
  const remote = new MediaStream([audio.receiver.track]);

  let down = false;
  let disconnectedTimer: ReturnType<typeof setTimeout> | undefined;
  const listeners = new Set<() => void>();
  let rejectOpen: (e: Error) => void = () => {};
  const linkOpen = new Promise<void>((resolve, reject) => {
    rejectOpen = reject;
    if (dc.readyState === "open") resolve();
    else dc.addEventListener("open", () => resolve(), { once: true });
  });
  linkOpen.catch(() => {}); // a peer nobody waits on may still die

  const goDown = () => {
    if (down) return;
    down = true;
    clearTimeout(disconnectedTimer);
    rejectOpen(new Error("The direct link to eve didn't open."));
    for (const listener of [...listeners]) listener();
    listeners.clear();
  };
  pc.addEventListener("connectionstatechange", () => {
    const s = pc.connectionState;
    if (s === "failed" || s === "closed") goDown();
    else if (s === "disconnected") {
      clearTimeout(disconnectedTimer);
      disconnectedTimer = setTimeout(() => {
        if (pc.connectionState !== "connected") goDown();
      }, DISCONNECTED_MS);
    } else if (s === "connected") clearTimeout(disconnectedTimer);
  });
  dc.addEventListener("close", goDown);

  const close = () => {
    goDown();
    try {
      pc.close();
    } catch {
      // already closed
    }
  };

  try {
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    await gathered(pc, GATHER_MS);
    if (opts.signal?.aborted) throw new DOMException("aborted", "AbortError");
    const sdp = pc.localDescription?.sdp ?? "";
    if (!sdp || sdp.length > MAX_OFFER_CHARS) throw new Error("This browser's offer is too large for eve.");
    const answer = readRtcAnswer(await api("POST", "/api/web/rtc", { sdp }, opts.fetch, opts.signal));
    if (!answer) throw new Error("eve's answer to the direct link wasn't usable.");
    if (opts.signal?.aborted) throw new DOMException("aborted", "AbortError");
    await pc.setRemoteDescription({ type: "answer", sdp: answer.sdp });
    return {
      link,
      linkOpen,
      voice: answer.voice,
      remote,
      setMic: (track) => audio.sender.replaceTrack(track),
      channel: (label) => pc.createDataChannel(label, { ordered: true }),
      alive: () => !down && dc.readyState === "open" && pc.connectionState !== "failed",
      onDown(listener) {
        if (down) {
          queueMicrotask(listener);
          return () => {};
        }
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      close,
    };
  } catch (e) {
    close();
    throw e;
  }
}
