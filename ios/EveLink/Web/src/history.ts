// Horizontal History (docs/app-chat.md): eve's one conversation as topic
// cards. The daemon cuts the transcript into topics at pauses of more than
// 45 minutes (link_topics.py); this file only shapes, orders and dates them,
// and finds where a topic starts in the loaded chat.

export interface Topic {
  id: string;
  first_id: string;
  last_id: string;
  started_at: number;
  ended_at: number;
  count: number;
  title: string;
  preview: string;
  bookmarked: boolean;
}

export interface TopicsView {
  topics: Topic[];
  bookmarked: Topic[];
}

const ID = /^(\d{4}-\d{2}-\d{2}):(\d{1,7})$/;

function topic(raw: unknown): Topic | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const str = (k: string) => (typeof r[k] === "string" ? (r[k] as string) : "");
  const num = (k: string) => (typeof r[k] === "number" && Number.isFinite(r[k]) ? (r[k] as number) : 0);
  if (!ID.test(str("id")) || !ID.test(str("first_id")) || !ID.test(str("last_id")) || !str("title")) return null;
  return {
    id: str("id"),
    first_id: str("first_id"),
    last_id: str("last_id"),
    started_at: num("started_at"),
    ended_at: num("ended_at"),
    count: Math.max(1, Math.round(num("count"))),
    title: str("title"),
    preview: str("preview"),
    bookmarked: r.bookmarked === true,
  };
}

/** The daemon's answer, checked: anything malformed is dropped, never shown. */
export function parseTopics(raw: unknown): TopicsView {
  const r = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const list = (k: string) => (Array.isArray(r[k]) ? (r[k] as unknown[]) : []).map(topic).filter((t): t is Topic => !!t);
  return { topics: list("topics"), bookmarked: list("bookmarked") };
}

/** Saved first, then the recent ones that are not saved: each topic once. */
export function cards(view: TopicsView): { saved: Topic[]; recent: Topic[] } {
  const savedIds = new Set(view.bookmarked.map((t) => t.id));
  return { saved: view.bookmarked, recent: view.topics.filter((t) => !savedIds.has(t.id)) };
}

/** Set a topic's star in both lists (the optimistic update, and its undo). */
export function withBookmark(view: TopicsView, id: string, on: boolean): TopicsView {
  const mark = (t: Topic) => (t.id === id ? { ...t, bookmarked: on } : t);
  const topics = view.topics.map(mark);
  const found = topics.find((t) => t.id === id) ?? view.bookmarked.find((t) => t.id === id);
  const rest = view.bookmarked.filter((t) => t.id !== id);
  return { topics, bookmarked: on && found ? [{ ...found, bookmarked: true }, ...rest] : rest };
}

/** "now", "12 min", "3 h", "Tue", "Sep 28": short, like a phone's message list. */
export function when(at: number, now: number = Date.now() / 1000): string {
  const s = Math.max(0, now - at);
  if (s < 90) return "now";
  if (s < 3600) return `${Math.round(s / 60)} min`;
  if (s < 20 * 3600) return `${Math.round(s / 3600)} h`;
  const d = new Date(at * 1000);
  if (s < 6 * 86400) return d.toLocaleDateString(undefined, { weekday: "short" });
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function key(id: string): [string, number] | null {
  const m = ID.exec(id);
  return m ? [m[1]!, Number(m[2])] : null;
}

function cmp(a: [string, number], b: [string, number]): number {
  return a[0] === b[0] ? a[1] - b[1] : a[0] < b[0] ? -1 : 1;
}

/** Whether a message id falls inside a topic (between its first and last message). */
export function inTopic(id: string, t: Topic): boolean {
  const k = key(id);
  const lo = key(t.first_id);
  const hi = key(t.last_id);
  return !!k && !!lo && !!hi && cmp(k, lo) >= 0 && cmp(k, hi) <= 0;
}

/** The first loaded message id of a topic, in chat order, or null when none of it is loaded. */
export function startOf(t: Topic, loaded: string[]): string | null {
  return loaded.find((id) => inTopic(id, t)) ?? null;
}

/** What the owner sends to pick a topic back up when it is no longer on the page. */
export function pickUp(t: Topic): string {
  return `Let's pick up "${t.title}" again.`;
}
