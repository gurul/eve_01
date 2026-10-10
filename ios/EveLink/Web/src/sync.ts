// Mapping between the daemon's transcript lines and OpenUI messages, and the
// ledger that keeps the live long poll from duplicating what a turn streams.
//
// eve has ONE conversation, so there is one thread. History comes from
// GET /api/link/messages; afterwards a long poll brings lines eve sends on
// its own (reminders, questions, replies to a Telegram turn). A turn started
// in the app streams its reply through /api/link/turn, and the same lines
// also reach the transcript, so the poll sees them again. The ledger drops
// those second copies:
//   - the owner line of an app turn matches the text the turn sent;
//   - eve lines match, in order, the text the turn streamed.
// While a turn is still streaming, eve lines are held and judged once the
// run has settled (flushIfIdle, called when the store's isRunning goes false):
// the stream's end comes before OpenUI has put the reply in the store, and a
// held line released then landed between the owner's message and the reply.
//
// A picture or file eve sends mid-turn (take_photo, a screenshot, a file)
// is a "media" line with attachments. The stream carries words only, so it is
// never claimed as a copy: it is held like any other line and, once the run
// settles, goes in just before the streamed reply (sink.insertBeforeReply), as
// its own message, where the transcript has it and history shows it. One that
// came in after the reply's own line, or once the stream ended, is after the
// reply in the transcript too, and is appended.
//
// A reply that leaned on eve's memory carries "sources" (sources.ts). They
// are kept beside the message by id; a merged line adds its own to the
// bubble it joins, and a claimed copy of a streamed reply hands its sources to
// that reply (LedgerSink.attachSources), since its words are already shown.

import type { Message } from "@openuidev/react-headless";
import type { WireMessage } from "./nativeBridge";
import { ownerParts, sanitizeAttachments, setEveMedia, type Attachment } from "./attachments";
import { wrapAction } from "./replyParts";
import { sanitizeSources, sources as sourceStore, type Source, type SourceStore } from "./sources";

/** Consecutive eve lines closer than this become one bubble in history. */
export const MERGE_WINDOW_S = 120;
/** How long an app turn can still claim lines from the poll. */
export const CLAIM_TTL_MS = 10 * 60 * 1000;

export function norm(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

/**
 * A hidden marker between two assistant messages. AgentInterface groups a run
 * of assistant messages into one turn and shows only the last one as the
 * answer, so separate eve bubbles need a non-assistant message between them.
 * Its role renders nothing.
 */
export function separator(id: string): Message {
  return { id: `sep:${id}`, role: "system", content: "" };
}

// What ownerText (replyParts.ts) appends to a component action's human text.
const ACTION_CONTEXT = /\n\n(\["User clicked: [\s\S]*\])$/;

/**
 * An owner line as the bubble shows it. A component action went to the
 * daemon as its human text plus the context JSON (ownerText); read back, it
 * is wrapped again, so the bubble shows "Yes" and its form chip, as it did
 * live, never the raw JSON.
 */
export function ownerContent(text: string): string {
  const m = ACTION_CONTEXT.exec(text);
  if (!m) return text;
  try {
    const context = JSON.parse(m[1]!) as unknown;
    if (!Array.isArray(context)) return text;
    return wrapAction(text.slice(0, m.index), context);
  } catch {
    return text;
  }
}

/** A line's attachments: an owner's photos and files, or the picture or file eve sent ("media"). */
function lineAttachments(w: WireMessage): Attachment[] {
  return sanitizeAttachments(w.attachments);
}

/** An eve line's sources: the memory lines its reply leaned on (sources.ts). */
export function lineSources(w: WireMessage): Source[] {
  return w.role === "eve" ? sanitizeSources(w.sources) : [];
}

function toChat(w: WireMessage, text: string, store: SourceStore): Message {
  if (w.role !== "owner") {
    // eve's own picture or file: kept beside the message, which AssistantMessage draws above the caption
    setEveMedia(w.id, lineAttachments(w));
    // the memory lines it leaned on, drawn under it as "Sources"
    store.set(w.id, lineSources(w));
    return { id: w.id, role: "assistant", content: text };
  }
  // An attachment whose upload expired ("gone") shows as its name, without the picture.
  const attachments = sanitizeAttachments(
    Array.isArray(w.attachments)
      ? w.attachments.map((a) => (a && typeof a === "object" && (a as { gone?: unknown }).gone ? { ...a, thumb: undefined } : a))
      : [],
  );
  return { id: w.id, role: "user", content: ownerParts(ownerContent(text), attachments) } as Message;
}

function visible(w: WireMessage): boolean {
  if (w.kind === "tool") return false;
  if (typeof w.text !== "string") return false;
  if (!w.text.trim() && lineAttachments(w).length === 0) return false;
  return w.role === "owner" || w.role === "eve";
}

/**
 * Transcript lines -> OpenUI messages. Consecutive eve lines within
 * MERGE_WINDOW_S of each other merge into one assistant message (joined by a
 * blank line, the same as a streamed turn); further apart they stay separate
 * bubbles with a separator between. A line with a picture or file is always
 * its own message, never merged into or onto: its attachments are its own.
 */
export function mapLines(lines: WireMessage[], previous?: Message, store: SourceStore = sourceStore): Message[] {
  const out: Message[] = [];
  let last: { msg: Message; t: number; media: boolean } | null = null;
  let prevRole = previous?.role;
  for (const w of lines) {
    if (!visible(w)) continue;
    const media = lineAttachments(w).length > 0;
    if (
      w.role === "eve" &&
      !media &&
      last &&
      !last.media &&
      last.msg.role === "assistant" &&
      w.t - last.t <= MERGE_WINDOW_S
    ) {
      last.msg = { ...last.msg, content: `${last.msg.content ?? ""}\n\n${w.text}` } as Message;
      out[out.length - 1] = last.msg;
      store.add(last.msg.id, lineSources(w));       // the bubble it joined shows its sources too
      last.t = w.t;
      continue;
    }
    if (w.role === "eve" && (last ? last.msg.role === "assistant" : prevRole === "assistant")) {
      out.push(separator(w.id));
    }
    const msg = toChat(w, w.text, store);
    out.push(msg);
    last = { msg, t: w.t, media };
    prevRole = msg.role;
  }
  return out;
}

/** Pulls `choices[0].delta.content` out of OpenAI Chat Completions SSE text. */
export class SseText {
  private buffer = "";
  text = "";

  push(chunk: string): void {
    this.buffer += chunk;
    const lines = this.buffer.split("\n");
    this.buffer = lines.pop() ?? "";
    for (const line of lines) this.line(line);
  }

  end(): void {
    if (this.buffer) this.line(this.buffer);
    this.buffer = "";
  }

  private line(line: string): void {
    if (!line.startsWith("data:")) return;
    const data = line.slice(5).trim();
    if (!data || data === "[DONE]") return;
    try {
      const json = JSON.parse(data) as { choices?: { delta?: { content?: unknown } }[] };
      const content = json.choices?.[0]?.delta?.content;
      if (typeof content === "string") this.text += content;
    } catch {
      // keep-alives and partial junk are ignored
    }
  }
}

interface Claim {
  sentText: string;
  ownerClaimed: boolean;
  reply: SseText;
  consumed: number;
  ended: boolean;
  startedAt: number;
}

export interface LedgerSink {
  /** Append messages to the thread (functional append, never a replace). */
  append(...messages: Message[]): void;
  /** Role of the last message in the thread, to place separators. */
  lastRole(): Message["role"] | undefined;
  /** No run of the page's own is in flight or settling (default: true). */
  idle?(): boolean;
  /**
   * Put messages just before the page's own streamed reply: the thread's
   * last message, when it is an assistant message after the last user
   * message. False, with nothing changed, when there is no such reply.
   */
  insertBeforeReply?(...messages: Message[]): boolean;
  /**
   * The poll's copy of the page's own streamed reply carried sources: put
   * them on that reply (the thread's last assistant message after the last
   * user message). Nothing when there is no such reply.
   */
  attachSources?(sources: Source[]): void;
}

export class Ledger {
  private seen = new Set<string>();
  private claims: Claim[] = [];
  private held: WireMessage[] = [];
  // Held media lines that came in while a reply was streaming, before its
  // own line: they go before the streamed reply, not after it.
  private beforeReply = new Set<string>();

  constructor(
    private readonly sink: LedgerSink,
    private readonly now: () => number = () => Date.now(),
  ) {}

  /** Ids already on screen (history load). */
  markSeen(lines: WireMessage[]): void {
    for (const w of lines) this.seen.add(w.id);
  }

  /** An app turn started; returns the hooks that feed its streamed text. */
  beginTurn(sentText: string): { bytes: (text: string) => void; end: () => void; hasText: () => boolean } {
    const claim: Claim = {
      sentText: norm(sentText),
      ownerClaimed: false,
      reply: new SseText(),
      consumed: 0,
      ended: false,
      startedAt: this.now(),
    };
    this.claims.push(claim);
    return {
      bytes: (text) => claim.reply.push(text),
      // Ended, not flushed: the held lines wait for flushIfIdle.
      end: () => {
        if (claim.ended) return;
        claim.ended = true;
        claim.reply.end();
      },
      /** The reply has words (a ": ping" keep-alive adds none). */
      hasText: () => claim.reply.text.length > 0,
    };
  }

  get streaming(): boolean {
    return this.claims.some((c) => !c.ended);
  }

  /** New lines from the long poll, oldest first. */
  ingest(lines: WireMessage[]): void {
    this.expire();
    const fresh: WireMessage[] = [];
    // While a reply streams, its own transcript line is written at the turn's
    // end: a media line before any eve words in this batch precedes it.
    let beforeReply = this.streaming;
    for (const w of lines) {
      if (this.seen.has(w.id)) continue;
      this.seen.add(w.id);
      if (!visible(w)) continue;
      if (w.role === "owner" && this.claimOwner(w)) continue;
      const media = lineAttachments(w).length > 0;
      if (w.role === "eve" && !media) beforeReply = false;
      if (media && beforeReply) this.beforeReply.add(w.id);
      fresh.push(w);
    }
    if (!this.isIdle() || this.held.length > 0) {
      // Hold everything until the run settles, so an outside line never
      // lands in the middle of a reply, or ahead of one not yet in the store.
      this.held.push(...fresh);
      this.flushIfIdle();
      return;
    }
    this.emit(fresh);
  }

  /**
   * Release the lines held during a turn. Call once the run has settled
   * (the store's isRunning is false), so the streamed reply is already in
   * the thread and an outside line lands after it.
   */
  flushIfIdle(): void {
    if (!this.isIdle() || this.held.length === 0) return;
    const held = this.held;
    this.held = [];
    this.emit(held);
  }

  private isIdle(): boolean {
    return !this.streaming && (this.sink.idle?.() ?? true);
  }

  private emit(lines: WireMessage[]): void {
    let keep = lines.filter((w) => !(w.role === "eve" && this.claimReply(w)));
    const early = keep.filter((w) => this.beforeReply.has(w.id));
    for (const w of lines) this.beforeReply.delete(w.id);
    if (early.length > 0 && this.sink.insertBeforeReply) {
      // after the owner's message, then a separator: the reply that follows
      // is another assistant message, and needs one between them
      const msgs = mapLines(early, { role: "user" } as Message);
      msgs.push(separator(`reply:${early[early.length - 1]!.id}`));
      if (this.sink.insertBeforeReply(...msgs)) keep = keep.filter((w) => !early.includes(w));
    }
    if (keep.length === 0) return;
    const prev = this.sink.lastRole();
    const msgs = mapLines(keep, prev ? ({ role: prev } as Message) : undefined);
    if (msgs.length) this.sink.append(...msgs);
  }

  private claimOwner(w: WireMessage): boolean {
    const text = norm(w.text);
    const claim = this.claims.find((c) => !c.ownerClaimed && c.sentText === text);
    if (!claim) return false;
    claim.ownerClaimed = true;
    return true;
  }

  private claimReply(w: WireMessage): boolean {
    // A picture or file never came through the stream (it carries words only),
    // so its line is never a copy of one, whatever its caption says.
    if (lineAttachments(w).length > 0) return false;
    const text = norm(w.text);
    if (!text) return false;
    for (const claim of this.claims) {
      const reply = norm(claim.reply.text);
      const at = reply.indexOf(text, claim.consumed);
      if (at !== -1) {
        claim.consumed = at + text.length;
        // the words are on screen already; what memory they leaned on is not
        const found = lineSources(w);
        if (found.length > 0) this.sink.attachSources?.(found);
        return true;
      }
    }
    return false;
  }

  private expire(): void {
    const cutoff = this.now() - CLAIM_TTL_MS;
    this.claims = this.claims.filter((c) => !c.ended || c.startedAt >= cutoff);
  }
}
