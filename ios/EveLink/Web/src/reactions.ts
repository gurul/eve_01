// eve's reaction on the owner's messages, as in Telegram (docs/app-chat.md,
// "Reactions"): 👀 while it reads a photo or file, ✍ a note kept, 🏆 a fact
// starred, 👍 typed to Claude or Codex. One per message; a new one replaces
// the old, and none is shown when it is taken off.
//
// The daemon keys a reaction by the app message's id ("rid"): for a turn sent
// from this page that is the client_id it was sent with (llm.ts links the
// bubble to it), and history rows carry their own "rid". Every
// /api/link/messages answer brings the full set ("reactions"), so a reaction
// that changes on a bubble already shown reaches it on the next poll.

import { useCallback, useSyncExternalStore } from "react";
import type { MessagesPayload, WireMessage } from "./nativeBridge";

/** A reaction is one emoji; a ZWJ sequence with modifiers fits in this many UTF-16 units. */
export const MAX_REACTION_CHARS = 16;
const RID = /^[A-Za-z0-9_-]{8,64}$/;
// Emoji pictographs, skin-tone modifiers, the emoji variation selector, the
// keycap mark and the zero-width joiner, and nothing else: no letters, digits,
// spaces or markup ever reach the badge.
const EMOJI_ONLY = /^(?:\p{Extended_Pictographic}|\p{Emoji_Modifier}|\u{FE0F}|\u{20E3}|\u{200D})+$/u;
const PICTOGRAPH = /\p{Extended_Pictographic}/u;
const MAX_KEPT = 400;

/** `raw` when it is a short emoji string, else "". */
export function sanitizeReaction(raw: unknown): string {
  if (typeof raw !== "string" || raw.length === 0 || raw.length > MAX_REACTION_CHARS) return "";
  return EMOJI_ONLY.test(raw) && PICTOGRAPH.test(raw) ? raw : "";
}

export function isRid(raw: unknown): raw is string {
  return typeof raw === "string" && RID.test(raw);
}

function cleanSet(raw: unknown): Map<string, string> | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const out = new Map<string, string>();
  for (const [rid, emoji] of Object.entries(raw as Record<string, unknown>)) {
    if (out.size >= MAX_KEPT) break;
    const clean = sanitizeReaction(emoji);
    if (isRid(rid) && clean) out.set(rid, clean);
  }
  return out;
}

export class ReactionStore {
  private ridOf = new Map<string, string>(); // message id -> rid
  private byRid = new Map<string, string>(); // rid -> emoji
  private listeners = new Set<() => void>();

  /** The bubble `messageId` is the daemon's message `rid`. */
  link(messageId: string | undefined, rid: unknown): void {
    if (!messageId || !isRid(rid) || this.ridOf.get(messageId) === rid) return;
    this.ridOf.set(messageId, rid);
    if (this.ridOf.size > MAX_KEPT * 2) this.ridOf.delete(this.ridOf.keys().next().value!);
    if (this.byRid.has(rid)) this.notify();
  }

  /** One /api/link/messages answer: its rows' ids, and the reactions it carries. */
  ingest(payload: Pick<MessagesPayload, "messages" | "reactions">): void {
    let changed = false;
    for (const w of payload.messages) {
      if (w.role !== "owner" || !isRid(w.rid) || this.ridOf.get(w.id) === w.rid) continue;
      this.ridOf.set(w.id, w.rid);
      changed = true;
    }
    const set = cleanSet(payload.reactions);
    if (set) {
      changed = changed || !sameMap(set, this.byRid);
      this.byRid = set;
    } else {
      // an older daemon: only the rows' own reactions
      for (const w of payload.messages) changed = this.put(w) || changed;
    }
    if (changed) this.notify();
  }

  private put(w: WireMessage): boolean {
    const emoji = sanitizeReaction(w.reaction);
    if (w.role !== "owner" || !isRid(w.rid) || !emoji || this.byRid.get(w.rid) === emoji) return false;
    this.byRid.set(w.rid, emoji);
    return true;
  }

  /** The reaction on the bubble `messageId`, or "". */
  reactionOf(messageId: string | undefined): string {
    const rid = messageId ? this.ridOf.get(messageId) : undefined;
    return (rid && this.byRid.get(rid)) || "";
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private notify(): void {
    for (const listener of this.listeners) listener();
  }
}

function sameMap(a: Map<string, string>, b: Map<string, string>): boolean {
  if (a.size !== b.size) return false;
  for (const [k, v] of a) if (b.get(k) !== v) return false;
  return true;
}

/** The page's one store (the bubbles read it; storage.ts and llm.ts fill it). */
export const reactions = new ReactionStore();

/** The reaction on one bubble, re-rendered when it changes. */
export function useReaction(messageId: string | undefined, store: ReactionStore = reactions): string {
  const read = useCallback(() => store.reactionOf(messageId), [store, messageId]);
  return useSyncExternalStore(store.subscribe, read, read);
}
