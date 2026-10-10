// The JS half of the Eve Link chat bridge (THE CONTRACT, part 2).
//
// JS never touches the network. Everything goes to native through
// window.webkit.messageHandlers.eve.postMessage, and native answers by
// calling window.eveNative.* with evaluateJavaScript. The link token stays
// in native code.

import { parseNativeAttachments, type Attachment } from "./attachments";
import { sameInsets, sanitizeInsets, type Insets } from "./insets";

export type HapticStyle = "light" | "soft" | "success";

export type OutboundMessage =
  | { type: "ready" }
  | { type: "turn"; id: string; text: string; client_id: string; attachments?: string[] }
  | { type: "cancel"; id: string }
  | { type: "messages"; id: string; after?: string; wait: number }
  | { type: "haptic"; style: HapticStyle }
  | { type: "openURL"; url: string }
  /** Open one attachment full size: native fetches GET /api/link/file/<id> and shows Quick Look. */
  | { type: "openAttachment"; id: string; name: string; kind: string };

export type WireRole = "owner" | "eve";
export type WireVia = "app" | "telegram" | "voice" | "other";
/** "media": a picture or file eve sent, its caption as the text (maybe empty), in `attachments`. */
export type WireKind = "say" | "tool" | "image" | "media";

export interface WireMessage {
  id: string;
  role: WireRole;
  text: string;
  t: number;
  via: WireVia;
  kind: WireKind;
  /** An owner line's photos and files, or eve's own on a "media" line (attachments.ts sanitizes it). */
  attachments?: unknown;
  /** An owner line's app message id, which its reaction is keyed by (reactions.ts). */
  rid?: unknown;
  /** eve's reaction on it, if any (reactions.ts sanitizes it). */
  reaction?: unknown;
  /** An eve reply's memory sources, [{kind, text, when?}] (sources.ts sanitizes them). */
  sources?: unknown;
}

export interface MessagesPayload {
  messages: WireMessage[];
  cursor: string;
  busy: boolean;
  /** Every reaction the daemon keeps, by rid (reactions.ts sanitizes it). */
  reactions?: unknown;
}

/** What native calls, installed as window.eveNative. */
export interface EveNative {
  onChunk(id: string, text: string): void;
  onDone(id: string): void;
  onError(id: string, message: string): void;
  onMessages(id: string, json: string): void;
  /** The composer's words, and (as JSON) what native uploaded with them. */
  send(text: string, attachments?: string): void;
  setBusy(busy: boolean): void;
  scrollToBottom(): void;
  /** Where the native top bar and composer cover the page, in CSS px (insets.ts). */
  setInsets(insets: Insets): void;
}

export interface TurnHooks {
  /** Raw SSE text, in order, for the dedupe ledger. */
  onBytes?: (text: string) => void;
  /** The stream is over: done, error, or cancelled. */
  onEnd?: () => void;
}

export interface OpenTurnParams {
  text: string;
  clientId: string;
  signal: AbortSignal;
  hooks?: TurnHooks;
  /** Upload ids (POST /api/link/upload), sent with the turn. */
  attachments?: string[];
}

export type SendHandler = (text: string, attachments: Attachment[]) => void;

export type Post = (message: OutboundMessage) => void;

export function uuid(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === "function") return c.randomUUID();
  // Custom-scheme pages may not be a secure context; getRandomValues still works.
  const b = new Uint8Array(16);
  c.getRandomValues(b);
  b[6] = (b[6]! & 0x0f) | 0x40;
  b[8] = (b[8]! & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

interface TurnEntry {
  controller: ReadableStreamDefaultController<Uint8Array>;
  hooks: TurnHooks;
}

interface RequestEntry {
  resolve: (payload: MessagesPayload) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export interface Bridge {
  /** The object native drives. */
  native: EveNative;
  post: Post;
  /** Start a turn; the Response body is fed by onChunk/onDone/onError. */
  openTurn(params: OpenTurnParams): Response;
  /** GET /api/link/messages through native. */
  requestMessages(after: string | undefined, wait: number, timeoutMs?: number): Promise<MessagesPayload>;
  /** Who handles native's send(text). Calls made before this is set are queued. */
  setSendHandler(handler: SendHandler | null): void;
  onBusy(listener: (busy: boolean) => void): () => void;
  setScrollHandler(handler: (() => void) | null): void;
  /** Native's insets: the listener hears the last ones at once (if any), then every change. */
  onInsets(listener: (insets: Insets) => void): () => void;
}

function parsePayload(json: string): MessagesPayload {
  const raw = JSON.parse(json) as Partial<MessagesPayload> | null;
  if (!raw || !Array.isArray(raw.messages)) throw new Error("bad messages payload");
  return {
    messages: raw.messages,
    cursor: typeof raw.cursor === "string" ? raw.cursor : "",
    busy: raw.busy === true,
    ...(raw.reactions !== undefined ? { reactions: raw.reactions } : {}),
  };
}

export function createBridge(post: Post): Bridge {
  const encoder = new TextEncoder();
  const turns = new Map<string, TurnEntry>();
  const requests = new Map<string, RequestEntry>();
  const busyListeners = new Set<(busy: boolean) => void>();
  const queuedSends: [string, Attachment[]][] = [];
  let sendHandler: SendHandler | null = null;
  let scrollHandler: (() => void) | null = null;
  const insetListeners = new Set<(insets: Insets) => void>();
  let lastInsets: Insets | null = null;

  const endTurn = (id: string): TurnEntry | undefined => {
    const entry = turns.get(id);
    if (!entry) return undefined;
    turns.delete(id);
    entry.hooks.onEnd?.();
    return entry;
  };

  const native: EveNative = {
    onChunk(id, text) {
      const entry = turns.get(id);
      if (!entry || typeof text !== "string" || text.length === 0) return;
      entry.hooks.onBytes?.(text);
      entry.controller.enqueue(encoder.encode(text));
    },
    onDone(id) {
      const entry = endTurn(id);
      if (entry) {
        try {
          entry.controller.close();
        } catch {
          // already closed
        }
        return;
      }
      // A messages request that native ended without a payload.
      const req = requests.get(id);
      if (req) {
        requests.delete(id);
        clearTimeout(req.timer);
        req.reject(new Error("messages request ended without a payload"));
      }
    },
    onError(id, message) {
      const error = new Error(message || "Eve is not reachable");
      const entry = endTurn(id);
      if (entry) {
        try {
          entry.controller.error(error);
        } catch {
          // already closed
        }
        return;
      }
      const req = requests.get(id);
      if (req) {
        requests.delete(id);
        clearTimeout(req.timer);
        req.reject(error);
      }
    },
    onMessages(id, json) {
      const req = requests.get(id);
      if (!req) return;
      requests.delete(id);
      clearTimeout(req.timer);
      try {
        req.resolve(parsePayload(json));
      } catch (e) {
        req.reject(e instanceof Error ? e : new Error(String(e)));
      }
    },
    send(text, json) {
      const attachments = parseNativeAttachments(json);
      if (typeof text !== "string" || (!text.trim() && attachments.length === 0)) return;
      if (sendHandler) sendHandler(text, attachments);
      else queuedSends.push([text, attachments]);
    },
    setBusy(busy) {
      for (const listener of busyListeners) listener(!!busy);
    },
    scrollToBottom() {
      scrollHandler?.();
    },
    setInsets(raw) {
      const insets = sanitizeInsets(raw);
      if (!insets || sameInsets(insets, lastInsets)) return;
      lastInsets = insets;
      for (const listener of insetListeners) listener(insets);
    },
  };

  return {
    native,
    post,

    openTurn({ text, clientId, signal, hooks = {}, attachments = [] }) {
      const id = uuid();
      let controller!: ReadableStreamDefaultController<Uint8Array>;
      const stream = new ReadableStream<Uint8Array>({
        start(c) {
          controller = c;
        },
        cancel() {
          // The reader gave up (e.g. the run was aborted): tell native to stop.
          if (endTurn(id)) post({ type: "cancel", id });
        },
      });
      turns.set(id, { controller, hooks });

      const abort = () => {
        if (!endTurn(id)) return;
        post({ type: "cancel", id });
        try {
          controller.close();
        } catch {
          // already closed
        }
      };
      if (signal.aborted) {
        abort();
      } else {
        signal.addEventListener("abort", abort, { once: true });
        post(
          attachments.length
            ? { type: "turn", id, text, client_id: clientId, attachments }
            : { type: "turn", id, text, client_id: clientId },
        );
      }

      return new Response(stream, {
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
      });
    },

    requestMessages(after, wait, timeoutMs = (wait + 15) * 1000) {
      const id = uuid();
      return new Promise<MessagesPayload>((resolve, reject) => {
        const timer = setTimeout(() => {
          if (!requests.delete(id)) return;
          post({ type: "cancel", id });
          reject(new Error("messages request timed out"));
        }, timeoutMs);
        requests.set(id, { resolve, reject, timer });
        const msg: OutboundMessage =
          after === undefined ? { type: "messages", id, wait } : { type: "messages", id, after, wait };
        post(msg);
      });
    },

    setSendHandler(handler) {
      sendHandler = handler;
      if (handler) {
        while (queuedSends.length) handler(...queuedSends.shift()!);
      }
    },

    onBusy(listener) {
      busyListeners.add(listener);
      return () => busyListeners.delete(listener);
    },

    setScrollHandler(handler) {
      scrollHandler = handler;
    },

    onInsets(listener) {
      insetListeners.add(listener);
      if (lastInsets) listener(lastInsets);
      return () => insetListeners.delete(listener);
    },
  };
}

interface WebkitWindow {
  webkit?: { messageHandlers?: { eve?: { postMessage(message: unknown): void } } };
}

/** Post to the native message handler; a no-op outside the app. */
export const postToNative: Post = (message) => {
  const handler = (globalThis as unknown as WebkitWindow).webkit?.messageHandlers?.eve;
  handler?.postMessage(message);
};
