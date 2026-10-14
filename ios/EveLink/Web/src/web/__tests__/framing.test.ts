import { describe, expect, it } from "vitest";
import {
  decodeClientFrame,
  decodeServerFrame,
  encodeCancel,
  encodeData,
  encodeEnd,
  encodeErr,
  encodeHead,
  encodeReq,
  fitsChannel,
  MAX_FRAME_BODY,
  MAX_REQ_BODY,
  nextId,
} from "../framing";

const utf8 = new TextEncoder();
const text = new TextDecoder();

describe("link framing", () => {
  it("framing round trip: a req and a cancel, and every frame the Mac sends, survive encode and decode", () => {
    const req = encodeReq({
      id: 7,
      m: "POST",
      p: "/api/link/turn",
      q: "",
      h: { "Content-Type": "application/json", Accept: "text/event-stream" },
      b: JSON.stringify({ text: "héllo 👋", client_id: "c-1", grade: "auto" }),
    });
    expect(decodeClientFrame(req)).toEqual({
      t: "req",
      id: 7,
      m: "POST",
      p: "/api/link/turn",
      q: "",
      h: { "content-type": "application/json", accept: "text/event-stream" },
      b: JSON.stringify({ text: "héllo 👋", client_id: "c-1", grade: "auto" }),
    });
    expect(decodeClientFrame(encodeCancel(7))).toEqual({ t: "cancel", id: 7 });

    expect(decodeServerFrame(encodeHead(7, 200, { "Content-Type": "text/event-stream" }))).toEqual({
      t: "head",
      id: 7,
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
    const body = utf8.encode('data: {"choices":[{"index":0,"delta":{"content":"hi"}}]}\n\n');
    const [frame] = encodeData(7, body);
    const decoded = decodeServerFrame(frame!.buffer);
    expect(decoded?.t).toBe("data");
    if (decoded?.t !== "data") throw new Error("expected data");
    expect(decoded.id).toBe(7);
    expect(text.decode(decoded.bytes)).toBe(text.decode(body));
    expect(decodeServerFrame(encodeEnd(7))).toEqual({ t: "end", id: 7 });
    expect(decodeServerFrame(encodeErr(7))).toEqual({ t: "err", id: 7 });
  });

  it("puts the id in the first 4 bytes, big-endian, and decodes ids up to 2^32 - 1", () => {
    const [frame] = encodeData(0x01020304, new Uint8Array([9]));
    expect(Array.from(frame!.subarray(0, 5))).toEqual([1, 2, 3, 4, 9]);
    const [top] = encodeData(0xffffffff, new Uint8Array([1]));
    expect(decodeServerFrame(top)).toMatchObject({ t: "data", id: 0xffffffff });
    // a view into a larger buffer decodes from its own offset
    const big = new Uint8Array(16);
    big.set(frame!, 8);
    expect(decodeServerFrame(big.subarray(8, 8 + frame!.length))).toMatchObject({ t: "data", id: 0x01020304 });
  });

  it("splits a body into frames of at most 16,384 bytes", () => {
    const body = new Uint8Array(MAX_FRAME_BODY * 2 + 5).map((_, i) => i % 251);
    const frames = encodeData(3, body);
    expect(frames.map((f) => f.length - 4)).toEqual([MAX_FRAME_BODY, MAX_FRAME_BODY, 5]);
    expect(encodeData(3, new Uint8Array(0))).toEqual([]);
  });

  it("ignores anything malformed", () => {
    for (const bad of [
      "",
      "not json",
      "[]",
      JSON.stringify({ t: "head", id: 1, s: 99 }),
      JSON.stringify({ t: "head", id: -1, s: 200 }),
      JSON.stringify({ t: "head", id: 1.5, s: 200 }),
      JSON.stringify({ t: "end" }),
      JSON.stringify({ t: "what", id: 1 }),
      new Uint8Array([0, 0, 1]),
      42,
      null,
    ]) {
      expect(decodeServerFrame(bad)).toBeNull();
    }
    expect(decodeClientFrame(JSON.stringify({ t: "req", id: 1, m: "GET", p: "/api/link/jobs", q: "", h: {}, b: "" }))).toBeNull();
    expect(decodeClientFrame(JSON.stringify({ t: "req", id: 1, m: "PUT", p: "/api/link/turn", q: "", h: {}, b: "" }))).toBeNull();
  });

  it("keeps a header's string values only, names lowercased; an err's reason is kept short", () => {
    expect(decodeServerFrame(JSON.stringify({ t: "head", id: 2, s: 503, h: { "X-A": "1", n: 5 } }))).toEqual({
      t: "head",
      id: 2,
      status: 503,
      headers: { "x-a": "1" },
    });
    const err = decodeServerFrame(JSON.stringify({ t: "err", id: 2, e: "x".repeat(1000) }));
    expect(err).toMatchObject({ t: "err", id: 2 });
    if (err?.t !== "err") throw new Error("expected err");
    expect(err.error?.length).toBe(300);
  });

  it("counts a body in UTF-8 bytes: 60,000 fit, one more does not", () => {
    expect(fitsChannel("a".repeat(MAX_REQ_BODY))).toBe(true);
    expect(fitsChannel("a".repeat(MAX_REQ_BODY + 1))).toBe(false);
    // 20,000 three-byte characters are exactly 60,000 bytes; one more is over
    expect(fitsChannel("€".repeat(20_000))).toBe(true);
    expect(fitsChannel("€".repeat(20_001))).toBe(false);
  });

  it("ids are u32, never 0, and wrap", () => {
    expect(nextId(0)).toBe(1);
    expect(nextId(41)).toBe(42);
    expect(nextId(0xffffffff)).toBe(1);
  });
});
