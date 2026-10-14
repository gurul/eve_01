// The `link` DataChannel's frames (docs/plans/eve-lite.md, M6 "The link channel"): HTTP requests to the unchanged
// /api/link door (link_chat.LinkChat.serve), carried over WebRTC to the Mac.
//
//   browser -> Mac   text {"t":"req","id":n,"m":"GET|POST","p":"/api/link/turn|/api/link/messages","q","h","b"}
//                    text {"t":"cancel","id":n}
//   Mac -> browser   text {"t":"head","id":n,"s":status,"h":{...}}
//                    binary: a 4-byte big-endian request id, then at most 16,384 bytes of de-chunked body
//                    text {"t":"end","id":n} or {"t":"err","id":n}
//
// The Mac's side is mirrored here (encodeHead, encodeData, ...) so the round trip is tested in one place.

/** A request's body ("b"), counted in UTF-8 bytes. A larger one goes over HTTPS instead. */
export const MAX_REQ_BODY = 60_000;
/** The body bytes in one binary frame, after the 4-byte id. */
export const MAX_FRAME_BODY = 16_384;
/** Requests one channel carries at once; more go over HTTPS. */
export const MAX_IN_FLIGHT = 8;
/** The only paths the channel forwards; the Mac answers anything else with head 404. */
export const LINK_PATHS = ["/api/link/turn", "/api/link/messages"] as const;
export type LinkPath = (typeof LINK_PATHS)[number];
export type LinkMethod = "GET" | "POST";

const MAX_ID = 0xffff_ffff;
const encoder = new TextEncoder();

export interface ReqFrame {
  t: "req";
  id: number;
  m: LinkMethod;
  p: LinkPath;
  q: string;
  h: Record<string, string>;
  b: string;
}

export type ClientFrame = ReqFrame | { t: "cancel"; id: number };

export type ServerFrame =
  | { t: "head"; id: number; status: number; headers: Record<string, string> }
  | { t: "data"; id: number; bytes: Uint8Array }
  | { t: "end"; id: number }
  | { t: "err"; id: number; error?: string };

export function isLinkPath(p: unknown): p is LinkPath {
  return typeof p === "string" && (LINK_PATHS as readonly string[]).includes(p);
}

function isId(v: unknown): v is number {
  return typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= MAX_ID;
}

/** The next request id after `id`: u32, never 0 (0 is left free for "none"). */
export function nextId(id: number): number {
  return id >= MAX_ID ? 1 : id + 1;
}

/** Whether `body` fits one req frame. */
export function fitsChannel(body: string): boolean {
  // A UTF-16 unit is at most 3 UTF-8 bytes, so short bodies need no encoding to know.
  if (body.length * 3 <= MAX_REQ_BODY) return true;
  return encoder.encode(body).length <= MAX_REQ_BODY;
}

/** Header names lowercased, values strings; anything else dropped. */
export function cleanHeaders(raw: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (!raw || typeof raw !== "object") return out;
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof v === "string" && k) out[k.toLowerCase()] = v;
  }
  return out;
}

/** A req frame. `p` is one of LINK_PATHS for a real request; anything else gets head 404 (linkChannel's probe). */
export function encodeReq(f: Omit<ReqFrame, "t" | "p"> & { p: string }): string {
  return JSON.stringify({ t: "req", id: f.id, m: f.m, p: f.p, q: f.q, h: cleanHeaders(f.h), b: f.b });
}

export function encodeCancel(id: number): string {
  return JSON.stringify({ t: "cancel", id });
}

/** A text frame from the page (the Mac's parse of it), or null for anything malformed. */
export function decodeClientFrame(text: string): ClientFrame | null {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return null;
  }
  if (!raw || typeof raw !== "object") return null;
  const f = raw as Record<string, unknown>;
  if (!isId(f.id)) return null;
  if (f.t === "cancel") return { t: "cancel", id: f.id };
  if (f.t !== "req" || (f.m !== "GET" && f.m !== "POST") || !isLinkPath(f.p)) return null;
  if (typeof f.q !== "string" || typeof f.b !== "string") return null;
  return { t: "req", id: f.id, m: f.m, p: f.p, q: f.q, h: cleanHeaders(f.h), b: f.b };
}

// ---- the Mac's frames ----------------------------------------------------------------------------------------

export function encodeHead(id: number, status: number, headers: Record<string, string> = {}): string {
  return JSON.stringify({ t: "head", id, s: status, h: cleanHeaders(headers) });
}

export function encodeEnd(id: number): string {
  return JSON.stringify({ t: "end", id });
}

export function encodeErr(id: number): string {
  return JSON.stringify({ t: "err", id });
}

/** A body as the binary frames the Mac sends: each the id (big-endian u32) and at most MAX_FRAME_BODY bytes. */
export function encodeData(id: number, body: Uint8Array): Uint8Array[] {
  const frames: Uint8Array[] = [];
  for (let at = 0; at < body.length; at += MAX_FRAME_BODY) {
    const part = body.subarray(at, at + MAX_FRAME_BODY);
    const frame = new Uint8Array(4 + part.length);
    new DataView(frame.buffer).setUint32(0, id, false);
    frame.set(part, 4);
    frames.push(frame);
  }
  return frames;
}

function bytesOf(data: ArrayBuffer | ArrayBufferView): Uint8Array {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
}

/** One message from the Mac, or null for anything malformed (it is then ignored). */
export function decodeServerFrame(data: unknown): ServerFrame | null {
  if (typeof data === "string") {
    let raw: unknown;
    try {
      raw = JSON.parse(data);
    } catch {
      return null;
    }
    if (!raw || typeof raw !== "object") return null;
    const f = raw as Record<string, unknown>;
    if (!isId(f.id)) return null;
    switch (f.t) {
      case "head": {
        const status = f.s;
        if (typeof status !== "number" || !Number.isInteger(status) || status < 100 || status > 599) return null;
        return { t: "head", id: f.id, status, headers: cleanHeaders(f.h) };
      }
      case "end":
        return { t: "end", id: f.id };
      case "err": {
        const said = typeof f.e === "string" ? f.e : typeof f.error === "string" ? f.error : undefined;
        return said ? { t: "err", id: f.id, error: said.slice(0, 300) } : { t: "err", id: f.id };
      }
      default:
        return null;
    }
  }
  if (data instanceof ArrayBuffer || ArrayBuffer.isView(data)) {
    const bytes = bytesOf(data as ArrayBuffer | ArrayBufferView);
    if (bytes.length < 4) return null;
    const id = new DataView(bytes.buffer, bytes.byteOffset, 4).getUint32(0, false);
    return { t: "data", id, bytes: bytes.subarray(4) };
  }
  return null;
}
