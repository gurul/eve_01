// webPost (docs/plans/eve-lite.md, M7): the bridge messages the chat page posts (nativeBridge.OutboundMessage),
// handled in the browser. App.tsx, llm.ts, storage.ts and nativeBridge.ts are the iOS page's own, unchanged; this
// file plays the part the iOS app's native code plays there (App/ChatWebView.swift's ChatModel and
// Shared/ChatTransport.swift), over the WebRTC link or HTTPS (transport.ts).
//
//   turn            POST /api/link/turn {text, client_id, grade, attachments?}: the SSE text to onChunk as it
//                   comes, then onDone or onError. A direct link that drops mid-reply carries on over HTTPS once:
//                   the same client_id joins the same turn, and the events already shown are skipped (sse.ts).
//   messages        GET /api/link/messages?after&wait, then onMessages
//   cancel          the request stops: a channel cancel, or the fetch aborted
//   openURL         http(s) in a new tab (noopener, noreferrer); mailto:, tel: and sms: to the system
//   openAttachment  a photo full size in the page, anything else in a new tab (Lightbox.tsx)
//   haptic          navigator.vibrate, where there is one
//   ready           the composer may send; the insets reach the page
//
// Unlike the iOS app, a page in the background keeps its streams: in eve lite nothing else would carry the rest of
// a reply (no Telegram), every line is in the transcript, and the long poll shows what a dropped stream missed.

import type { Insets } from "../insets";
import type { EveNative, HapticStyle, OutboundMessage, Post } from "../nativeBridge";
import { UNREACHABLE, type Where } from "./api";
import { LinkLost, type Sink } from "./linkChannel";
import { SseEvents } from "./sse";
import { Unreachable, type Handle, type Transport } from "./transport";

/** The composer's Think toggle: the daemon's grade for the turn (link_chat GRADES). */
export type Grade = "auto" | "think";

/** link_chat's limits, checked here first as native checks them (ChatBridgeMessage). */
export const MAX_TEXT = 4000;
const MAX_CLIENT_ID = 64;
const MAX_ATTACHMENTS = 4;
const UPLOAD_ID = /^[0-9a-f]{32}$/;
/** No byte for this long (the daemon pings every 15 s) is a dead stream, as native's URLSession timeout. */
export const TURN_IDLE_MS = 45_000;
/**
 * A turn is asked again (to carry on over HTTPS) only this soon after it started: the daemon knows a client_id
 * for link_chat.DEDUPE_SECS (10 minutes), and past that the same words would start a second turn.
 */
export const RESUME_WITHIN_MS = 5 * 60_000;
/** A messages page larger than this is not a page. */
const MAX_PAGE_BYTES = 16 * 1024 * 1024;
const MAX_ERROR_BYTES = 64 * 1024;

export const MAC_SIGNED_OUT = "This browser isn't signed in to eve. Open Eve from Applications on the Mac.";
export const DEVICE_UNPAIRED = "This device isn't paired with eve any more. Pair it again from the Mac.";
const MAC_UNREACHABLE = "eve isn't answering on this Mac. It may be restarting: try again in a few seconds.";

/** Unicode scalars, as the daemon counts (Python's len). */
export function scalars(text: string): number {
  let n = 0;
  for (const _ of text) n += 1;
  return n;
}

export interface Attachment {
  id: string;
  name: string;
  kind: string;
}

export interface HostOptions {
  where: Where;
  transport: Pick<Transport, "request">;
  /** The Think toggle, read when a turn starts. */
  grade(): Grade;
  /** A web page in a new tab, with noopener and noreferrer. */
  openWindow(url: string): void;
  /** A mailto:, tel: or sms: link, to the system's own handler. */
  openScheme(url: string): void;
  openAttachment(a: Attachment): void;
  vibrate?(pattern: number | number[]): void;
  /** A request answered 401: the device cookie is gone or revoked. */
  onAuthLost?(): void;
}

export interface HostState {
  /** The page said ready: the composer may send. */
  ready: boolean;
  /** A turn from this page is streaming: the send button is a stop button. */
  busy: boolean;
  /** The last turn's failure, for one line above the composer ("" when none). */
  problem: string;
  /** The conversation was empty when the page opened (null until it is known). */
  historyEmpty: boolean | null;
}

export interface WebHost {
  readonly post: Post;
  attach(native: EveNative): void;
  getState(): HostState;
  subscribe(listener: () => void): () => void;
  /** Where the shell's top bar and composer cover the page (insets.ts), now and on every change. */
  setInsets(insets: Insets): void;
  /** The Stop button: this page's turns end now, and the page treats each reply as finished (onDone). */
  stop(): void;
  clearProblem(): void;
  /** Everything stops, silently (the chat is going away). */
  dispose(): void;
}

interface Entry {
  kind: "turn" | "messages";
  handle: Handle | null;
  idle?: ReturnType<typeof setTimeout>;
  /** When the turn was first sent (Date.now()). */
  started: number;
}

const HAPTICS: Record<HapticStyle, number | number[]> = { light: 8, soft: 12, success: [10, 60, 14] };

function errorLine(body: string): string | undefined {
  try {
    const error = (JSON.parse(body) as { error?: unknown } | null)?.error;
    return typeof error === "string" && error.trim() ? error.trim().slice(0, 300) : undefined;
  } catch {
    return undefined;
  }
}

function joined(chunks: Uint8Array[]): string {
  const decoder = new TextDecoder();
  return chunks.map((c) => decoder.decode(c, { stream: true })).join("") + decoder.decode();
}

/** What a bad turn message is missing, or null when native would have sent it. */
function turnProblem(msg: Extract<OutboundMessage, { type: "turn" }>): string | null {
  const text = typeof msg.text === "string" ? msg.text.trim() : "";
  const ids = msg.attachments ?? [];
  if (!Array.isArray(ids) || ids.length > MAX_ATTACHMENTS || new Set(ids).size !== ids.length) {
    return "Four attachments at most.";
  }
  if (!ids.every((i) => typeof i === "string" && UPLOAD_ID.test(i))) return "An attachment didn't upload. Attach it again.";
  if (!text && ids.length === 0) return "There's nothing to send.";
  if (scalars(text) > MAX_TEXT) return `Too long: ${MAX_TEXT} characters at most.`;
  if (typeof msg.client_id !== "string" || !msg.client_id || msg.client_id.length > MAX_CLIENT_ID) {
    return "That message couldn't be sent. Send it again.";
  }
  return null;
}

export function createWebHost(opts: HostOptions): WebHost {
  let native: EveNative | null = null;
  let insets: Insets | null = null;
  let disposed = false;
  const open = new Map<string, Entry>();
  const listeners = new Set<() => void>();
  let state: HostState = { ready: false, busy: false, problem: "", historyEmpty: null };

  const set = (patch: Partial<HostState>) => {
    const next = { ...state, ...patch };
    if (
      next.ready === state.ready &&
      next.busy === state.busy &&
      next.problem === state.problem &&
      next.historyEmpty === state.historyEmpty
    ) {
      return;
    }
    state = next;
    for (const listener of [...listeners]) listener();
  };

  const turnsOpen = () => [...open.values()].some((e) => e.kind === "turn");
  const refreshBusy = () => {
    const busy = turnsOpen();
    if (busy === state.busy) return;
    set({ busy });
    native?.setBusy(busy);
  };

  const failureText = (status: number, body: string): string => {
    const said = errorLine(body);
    switch (status) {
      case 401:
        opts.onAuthLost?.();
        return opts.where === "mac" ? MAC_SIGNED_OUT : DEVICE_UNPAIRED;
      case 403:
        return "eve refused the request.";
      case 421:
        return "Open eve at the address the Mac shows.";
      case 503:
        return said ?? "eve's chat isn't running.";
      default:
        return said ?? `eve answered ${status}.`;
    }
  };

  const messageOf = (error: Error): string => {
    if (error instanceof Unreachable) {
      return opts.where === "mac" && error.message === UNREACHABLE ? MAC_UNREACHABLE : error.message;
    }
    if (error instanceof LinkLost) return "The direct link to eve dropped. Send that again.";
    return error.message || UNREACHABLE;
  };

  // ---- turn ----------------------------------------------------------------------------------------------

  const finishTurn = (id: string, entry: Entry, error: string | null) => {
    if (open.get(id) !== entry) return;
    open.delete(id);
    clearTimeout(entry.idle);
    refreshBusy();
    if (error === null) {
      native?.onDone(id);
    } else {
      native?.onError(id, error);
      set({ problem: error });
    }
  };

  const sendTurn = (id: string, entry: Entry, body: string, events: SseEvents, retried: boolean) => {
    let status = 0;
    let errorBody = "";
    const decoder = new TextDecoder();
    const live = () => open.get(id) === entry;
    // The direct link went quiet or dropped: once more over HTTPS, the same client_id, the shown events skipped.
    const again = (): boolean => {
      if (retried || !live() || (status !== 0 && status !== 200)) return false;
      if (Date.now() - entry.started > RESUME_WITHIN_MS) return false;
      entry.handle?.cancel();
      sendTurn(id, entry, body, new SseEvents(events.delivered), true);
      return true;
    };
    const armIdle = () => {
      clearTimeout(entry.idle);
      entry.idle = setTimeout(() => {
        if (!live() || again()) return;
        entry.handle?.cancel();
        finishTurn(id, entry, "eve stopped answering. If a reply comes, it will show here.");
      }, TURN_IDLE_MS);
    };
    const sink: Sink = {
      head(s) {
        if (!live()) return;
        status = s;
        armIdle();
      },
      data(bytes) {
        if (!live()) return;
        armIdle();
        const text = decoder.decode(bytes, { stream: true });
        if (status !== 200) {
          if (errorBody.length < MAX_ERROR_BYTES) errorBody += text;
          return;
        }
        const out = events.push(text);
        if (out) native?.onChunk(id, out);
      },
      end() {
        if (!live()) return;
        const tail = decoder.decode();
        if (status !== 200) {
          finishTurn(id, entry, failureText(status, errorBody + tail));
          return;
        }
        const out = events.push(tail) + events.end();
        if (out) native?.onChunk(id, out);
        finishTurn(id, entry, null);
      },
      fail(error) {
        if (!live()) return;
        if (error instanceof LinkLost && again()) return;
        finishTurn(id, entry, messageOf(error));
      },
    };
    armIdle();
    entry.handle = opts.transport.request(
      {
        method: "POST",
        path: "/api/link/turn",
        query: "",
        headers: { "content-type": "application/json", accept: "text/event-stream" },
        body,
      },
      sink,
      retried ? { via: "fetch" } : {},
    );
  };

  const turn = (msg: Extract<OutboundMessage, { type: "turn" }>) => {
    const problem = turnProblem(msg);
    if (problem) {
      native?.onError(msg.id, problem);
      set({ problem });
      return;
    }
    const ids = msg.attachments ?? [];
    const body = JSON.stringify({
      text: msg.text.trim(),
      client_id: msg.client_id,
      grade: opts.grade(),
      ...(ids.length ? { attachments: ids } : {}),
    });
    const entry: Entry = { kind: "turn", handle: null, started: Date.now() };
    open.get(msg.id)?.handle?.cancel();
    open.set(msg.id, entry);
    set({ problem: "", historyEmpty: false });
    refreshBusy();
    sendTurn(msg.id, entry, body, new SseEvents(), false);
  };

  // ---- messages --------------------------------------------------------------------------------------------

  const messages = (msg: Extract<OutboundMessage, { type: "messages" }>) => {
    const id = msg.id;
    const query = new URLSearchParams();
    if (typeof msg.after === "string" && msg.after) query.set("after", msg.after);
    if (typeof msg.before === "string" && msg.before) query.set("before", msg.before);   // infinite scroll
    const wait = Number.isFinite(msg.wait) ? Math.min(25, Math.max(0, Math.floor(msg.wait))) : 0;
    if (wait > 0) query.set("wait", String(wait));
    const entry: Entry = { kind: "messages", handle: null, started: Date.now() };
    open.get(id)?.handle?.cancel();
    open.set(id, entry);
    const live = () => open.get(id) === entry;
    const done = () => {
      open.delete(id);
    };
    let status = 0;
    let size = 0;
    const chunks: Uint8Array[] = [];
    const history = msg.after === undefined;
    entry.handle = opts.transport.request(
      { method: "GET", path: "/api/link/messages", query: query.toString(), headers: { accept: "application/json" }, body: "" },
      {
        head(s) {
          status = s;
        },
        data(bytes) {
          if (!live()) return;
          size += bytes.length;
          if (size > MAX_PAGE_BYTES) {
            entry.handle?.cancel();
            done();
            native?.onError(id, "eve's answer was too large.");
            return;
          }
          chunks.push(bytes);
        },
        end() {
          if (!live()) return;
          done();
          const text = joined(chunks);
          if (status !== 200) {
            native?.onError(id, failureText(status, text));
            return;
          }
          if (state.historyEmpty !== false) {
            try {
              const lines = (JSON.parse(text) as { messages?: unknown } | null)?.messages;
              if (Array.isArray(lines) && (lines.length > 0 || history)) set({ historyEmpty: lines.length === 0 });
            } catch {
              // the bridge reports a bad payload itself
            }
          }
          native?.onMessages(id, text);
        },
        fail(error) {
          if (!live()) return;
          done();
          native?.onError(id, messageOf(error));
        },
      },
    );
  };

  // ---- the rest ------------------------------------------------------------------------------------------------

  const openURL = (raw: string) => {
    let url: URL;
    try {
      url = new URL(raw);
    } catch {
      return;
    }
    if (url.protocol === "http:" || url.protocol === "https:") opts.openWindow(url.toString());
    else if (url.protocol === "mailto:" || url.protocol === "tel:" || url.protocol === "sms:") opts.openScheme(raw.trim());
  };

  const haptic = (style: HapticStyle) => {
    const pattern = HAPTICS[style];
    if (pattern !== undefined) opts.vibrate?.(pattern);
  };

  const post: Post = (msg) => {
    if (disposed || !native) return;
    switch (msg.type) {
      case "ready":
        set({ ready: true });
        if (insets) native.setInsets(insets);
        return;
      case "turn":
        return turn(msg);
      case "messages":
        return messages(msg);
      case "cancel": {
        const entry = open.get(msg.id);
        if (!entry) return;
        open.delete(msg.id);
        clearTimeout(entry.idle);
        entry.handle?.cancel();
        refreshBusy();
        return;
      }
      case "haptic":
        return haptic(msg.style);
      case "openURL":
        return openURL(msg.url);
      case "topics":
      case "bookmark":
        native.onError(msg.id, "History is not in eve lite yet");   // Horizontal History needs the app's composer
        return;
      case "openAttachment":
        if (UPLOAD_ID.test(msg.id)) opts.openAttachment({ id: msg.id, name: msg.name, kind: msg.kind });
        return;
    }
  };

  return {
    post,
    attach(n) {
      native = n;
      if (insets) n.setInsets(insets);
    },
    getState: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    setInsets(next) {
      insets = next;
      native?.setInsets(next);
    },
    stop() {
      for (const [id, entry] of [...open]) {
        if (entry.kind !== "turn") continue;
        entry.handle?.cancel();
        finishTurn(id, entry, null);
      }
    },
    clearProblem() {
      set({ problem: "" });
    },
    dispose() {
      disposed = true;
      for (const entry of open.values()) {
        clearTimeout(entry.idle);
        entry.handle?.cancel();
      }
      open.clear();
      listeners.clear();
    },
  };
}
