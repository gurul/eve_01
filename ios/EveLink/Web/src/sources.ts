// Sources under a reply (docs/app-chat.md, "Sources"): the memory lines an
// answer leaned on, as ChatGPT shows "Sources" below a response that used
// personal context. The daemon writes them on the reply's transcript line
// (memory_sources.py) and /api/link/messages gives that eve row
// "sources": [{kind, text, when?}].
//
// An assistant message's content is a string, so its sources are kept
// beside it, by message id, like eve's media (attachments.ts). History
// sets them when a line is mapped (sync.ts toChat). A reply streamed in this
// page has its own id; when the poll brings its transcript line, the ledger
// drops the copy and hands its sources to that streamed reply
// (LedgerSink.attachSources), so the store notifies and the bubble redraws.

import { useCallback, useSyncExternalStore } from "react";

export type SourceKind = "fact" | "remembered" | "said";

export interface Source {
  kind: SourceKind;
  text: string;
  when?: string;
}

export const MAX_SOURCES = 6;
export const MAX_SOURCE_CHARS = 200;
export const MAX_WHEN_CHARS = 40;
const KINDS: readonly SourceKind[] = ["fact", "remembered", "said"];
const MAX_KEPT = 400;

/** The words shown for a kind. */
export const KIND_LABEL: Record<SourceKind, string> = {
  fact: "Fact",
  remembered: "Remembered",
  said: "Said",
};

function clip(raw: string, cap: number): string {
  const s = raw.replace(/\s+/g, " ").trim();
  return s.length <= cap ? s : `${s.slice(0, cap - 1).trimEnd()}…`;
}

/** Well-formed sources only (a history row's), string fields, at most MAX_SOURCES, no repeats. */
export function sanitizeSources(raw: unknown): Source[] {
  if (!Array.isArray(raw)) return [];
  const out: Source[] = [];
  const seen = new Set<string>();
  for (const s of raw) {
    if (out.length >= MAX_SOURCES) break;
    if (!s || typeof s !== "object") continue;
    const { kind, text, when } = s as Record<string, unknown>;
    if (typeof kind !== "string" || !KINDS.includes(kind as SourceKind)) continue;
    if (typeof text !== "string") continue;
    const body = clip(text, MAX_SOURCE_CHARS);
    const key = `${kind}\u0000${body}`;
    if (!body || seen.has(key)) continue;
    seen.add(key);
    const src: Source = { kind: kind as SourceKind, text: body };
    if (typeof when === "string") {
      const w = clip(when, MAX_WHEN_CHARS);
      if (w) src.when = w;
    }
    out.push(src);
  }
  return out;
}

const NONE: Source[] = [];

export class SourceStore {
  private byId = new Map<string, Source[]>();
  private listeners = new Set<() => void>();

  /** The message `id`'s sources, replacing any it had (history). */
  set(id: string, sources: Source[]): void {
    const had = this.byId.get(id);
    if (sources.length === 0) {
      if (had && this.byId.delete(id)) this.notify();
      return;
    }
    if (had && same(had, sources)) return;
    this.byId.set(id, sources);
    if (this.byId.size > MAX_KEPT) this.byId.delete(this.byId.keys().next().value!);
    this.notify();
  }

  /** More sources onto message `id` (a merged line, a claimed copy), no repeats, at most MAX_SOURCES. */
  add(id: string, sources: Source[]): void {
    if (sources.length === 0) return;
    this.set(id, sanitizeSources([...(this.byId.get(id) ?? []), ...sources]));
  }

  sourcesOf(id: string | undefined): Source[] {
    return (id && this.byId.get(id)) || NONE;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private notify(): void {
    for (const listener of this.listeners) listener();
  }
}

function same(a: Source[], b: Source[]): boolean {
  return a.length === b.length && a.every((s, i) => s.kind === b[i]!.kind && s.text === b[i]!.text && s.when === b[i]!.when);
}

/** The page's one store (sync.ts fills it; AssistantMessage reads it). */
export const sources = new SourceStore();

/** The sources of one bubble, re-rendered when they change. */
export function useSources(messageId: string | undefined, store: SourceStore = sources): Source[] {
  const read = useCallback(() => store.sourcesOf(messageId), [store, messageId]);
  return useSyncExternalStore(store.subscribe, read, read);
}
