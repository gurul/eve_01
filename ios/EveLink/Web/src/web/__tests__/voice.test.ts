import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CONNECT_MS, endedLine, MIC_MODE, MIC_TAIL_MS, PING_MS, VOICE_TEXT, VoiceController, type VoicePeer } from "../voice";
import { FakePeer } from "./fakes";

class FakeTrack {
  readyState: "live" | "ended" = "live";
  stopped = false;
  stop() {
    this.readyState = "ended";
    this.stopped = true;
  }
}

function setup(opts: { capture?: boolean; voice?: boolean; peerFails?: boolean; micFails?: string } = {}) {
  const peer = new FakePeer(opts.voice ?? true);
  peer.openLink();
  const tracks: FakeTrack[] = [];
  let micGate: Promise<void> = Promise.resolve();
  let release = vi.fn();
  const deps = {
    peer: vi.fn(async (): Promise<VoicePeer> => {
      if (opts.peerFails) throw new Error("no link");
      return peer;
    }),
    release: (release = vi.fn()),
    mic: vi.fn(async () => {
      await micGate;
      if (opts.micFails) throw new DOMException("denied", opts.micFails);
      const t = new FakeTrack();
      tracks.push(t);
      return t as unknown as MediaStreamTrack;
    }),
    canCapture: () => opts.capture ?? true,
  };
  const voice = new VoiceController(deps);
  return {
    voice,
    peer,
    deps,
    tracks,
    release: () => release,
    holdMic: () => {
      let open!: () => void;
      micGate = new Promise((r) => (open = r));
      return () => open();
    },
    call: () => peer.calls.at(-1)!,
  };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("push-to-talk over the call channel", () => {
  it("asks for the microphone first, then opens the call; talk waits for the Mac's connected", async () => {
    const t = setup();
    await t.voice.start();
    expect(t.deps.mic).toHaveBeenCalledOnce();
    expect(t.tracks[0]!.stopped).toBe(MIC_MODE === "per-press"); // permission asked, capture not kept
    expect(t.voice.getState()).toMatchObject({ phase: "connecting", status: VOICE_TEXT.connecting });
    t.voice.press(); // not connected yet: nothing to press
    expect(t.call().sent).toEqual([]);
    t.call().connect();
    expect(t.voice.getState()).toMatchObject({ phase: "ready", status: VOICE_TEXT.ready });
    await vi.advanceTimersByTimeAsync(PING_MS);
    expect(t.call().types()).toEqual(["ping"]);
  });

  it("a press sends talk at once and puts a fresh microphone on the peer; the release sends done, then lets it go", async () => {
    const t = setup();
    await t.voice.start();
    t.call().connect();
    t.voice.press();
    expect(t.call().types()).toEqual(["talk"]);
    expect(t.voice.getState()).toMatchObject({ phase: "talking", status: VOICE_TEXT.listening });
    await vi.advanceTimersByTimeAsync(0);
    const mic = t.tracks.at(-1)!;
    expect(t.peer.mic).toBe(mic);
    t.voice.release();
    expect(t.call().types()).toEqual(["talk", "done"]);
    expect(t.voice.getState().phase).toBe("thinking");
    // the tail: the microphone's last audio still leaves before it stops (capture per press)
    await vi.advanceTimersByTimeAsync(MIC_TAIL_MS - 1);
    expect(mic.stopped).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(mic.stopped).toBe(true);
    expect(t.peer.mic).toBeNull();
  });

  it("a tap released before the microphone started sends done, and the late microphone is stopped unused", async () => {
    const t = setup();
    await t.voice.start();
    t.call().connect();
    const open = t.holdMic();
    t.voice.press();
    t.voice.release();
    expect(t.call().types()).toEqual(["talk", "done"]);
    open();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.tracks.at(-1)!.stopped).toBe(true);
    expect(t.peer.mic).toBeNull();
  });

  it("captions: what eve heard, and each sentence as it is said; a flush or a new press clears eve's", async () => {
    const t = setup();
    await t.voice.start();
    t.call().connect();
    t.call().mac({ type: "heard", text: "what's on today" });
    t.call().mac({ type: "state", state: "thinking" });
    expect(t.voice.getState()).toMatchObject({ heard: "what's on today", phase: "thinking", status: VOICE_TEXT.thinking });
    t.call().mac({ type: "state", state: "speaking" });
    t.call().mac({ type: "said", text: "Two meetings." });
    expect(t.voice.getState()).toMatchObject({ phase: "speaking", said: "Two meetings." });
    t.call().mac({ type: "flush" });
    expect(t.voice.getState().said).toBe("");
    t.call().mac({ type: "said", text: "Lunch at one." });
    t.voice.press(); // barge-in
    expect(t.voice.getState().said).toBe("");
    t.call().mac({ type: "said", text: "late sentence" }); // while the button is down: not shown
    expect(t.voice.getState().said).toBe("");
    t.voice.release();
    t.call().mac({ type: "state", state: "listening", note: "I didn't catch anything. Try again?" });
    expect(t.voice.getState()).toMatchObject({ phase: "ready", status: "I didn't catch anything. Try again?" });
  });

  it("the Mac ending the call shows why, and Call again opens a new one", async () => {
    const t = setup();
    await t.voice.start();
    t.call().connect();
    t.call().mac({ type: "ended", reason: "the call reached its hour" });
    expect(t.voice.getState()).toMatchObject({ phase: "ended", status: "The call ended: the call reached its hour." });
    expect(endedLine("Only eve's owner can call.")).toBe("Only eve's owner can call.");
    expect(endedLine("")).toBe(VOICE_TEXT.ended);
    expect(t.release()).toHaveBeenCalledOnce();
    await t.voice.start();
    expect(t.peer.calls).toHaveLength(2);
  });

  it("a call the Mac never answers is given up after 10 s, with the direct link's fix", async () => {
    const t = setup();
    await t.voice.start();
    await vi.advanceTimersByTimeAsync(CONNECT_MS - 1);
    expect(t.voice.getState().phase).toBe("connecting");
    await vi.advanceTimersByTimeAsync(1);
    expect(t.voice.getState()).toMatchObject({ phase: "unavailable", fix: "local-network", status: VOICE_TEXT.noLink });
    expect(t.call().closed).toBe(true);
    expect(t.release()).toHaveBeenCalledOnce();
  });

  it("a channel that drops says the call dropped", async () => {
    const t = setup();
    await t.voice.start();
    t.call().connect();
    t.call().close();
    expect(t.voice.getState()).toMatchObject({ phase: "ended", status: VOICE_TEXT.dropped });
  });

  it("stop hangs up: end is sent, the channel closes, the microphone goes, the peer is let go", async () => {
    const t = setup();
    await t.voice.start();
    t.call().connect();
    t.voice.press();
    await vi.advanceTimersByTimeAsync(0);
    t.voice.stop();
    expect(t.call().types()).toEqual(["talk", "done", "end"]);
    expect(t.call().closed).toBe(true);
    expect(t.tracks.at(-1)!.stopped).toBe(true);
    expect(t.release()).toHaveBeenCalledOnce();
    expect(t.voice.getState().phase).toBe("off");
  });

  it("says what is missing: a secure page, the microphone, the direct link, or voice on the Mac", async () => {
    const insecure = setup({ capture: false });
    await insecure.voice.start();
    expect(insecure.voice.getState()).toMatchObject({ phase: "unavailable", fix: "secure" });
    expect(insecure.deps.mic).not.toHaveBeenCalled();

    const denied = setup({ micFails: "NotAllowedError" });
    await denied.voice.start();
    expect(denied.voice.getState()).toMatchObject({ phase: "unavailable", fix: "microphone", status: VOICE_TEXT.micBlocked });
    expect(denied.deps.peer).not.toHaveBeenCalled();

    const noMic = setup({ micFails: "NotFoundError" });
    await noMic.voice.start();
    expect(noMic.voice.getState()).toMatchObject({ status: VOICE_TEXT.noMic });

    const blocked = setup({ peerFails: true });
    await blocked.voice.start();
    expect(blocked.voice.getState()).toMatchObject({ phase: "unavailable", fix: "local-network", status: VOICE_TEXT.noLink });

    const off = setup({ voice: false });
    await off.voice.start();
    expect(off.voice.getState()).toMatchObject({ phase: "unavailable", status: VOICE_TEXT.noVoice });
    expect(off.peer.calls).toHaveLength(0);
  });
});
