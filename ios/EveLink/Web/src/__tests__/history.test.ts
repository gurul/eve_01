import { describe, expect, it } from "vitest";
import { cards, inTopic, parseTopics, pickUp, startOf, when, withBookmark, type Topic } from "../history";
import { createBridge, type OutboundMessage } from "../nativeBridge";

const t = (id: string, last: string, extra: Partial<Topic> = {}): Topic => ({
  id,
  first_id: id,
  last_id: last,
  started_at: 1000,
  ended_at: 2000,
  count: 4,
  title: `topic ${id}`,
  preview: "",
  bookmarked: false,
  ...extra,
});

describe("Horizontal History", () => {
  it("keeps well-formed topics and drops the rest", () => {
    const good = t("2026-10-13:4", "2026-10-13:9");
    const view = parseTopics({
      topics: [good, { ...good, id: "../x" }, { ...good, title: "" }, null, "x"],
      bookmarked: "nope",
    });
    expect(view.topics).toEqual([good]);
    expect(view.bookmarked).toEqual([]);
    expect(parseTopics(null)).toEqual({ topics: [], bookmarked: [] });
  });

  it("shows saved topics first and each topic once", () => {
    const a = t("2026-10-13:0", "2026-10-13:3", { bookmarked: true });
    const b = t("2026-10-13:5", "2026-10-13:8");
    expect(cards({ topics: [b, a], bookmarked: [a] })).toEqual({ saved: [a], recent: [b] });
  });

  it("stars and unstars a topic in both lists", () => {
    const a = t("2026-10-13:0", "2026-10-13:3");
    const on = withBookmark({ topics: [a], bookmarked: [] }, a.id, true);
    expect(on.topics[0]!.bookmarked).toBe(true);
    expect(on.bookmarked.map((x) => x.id)).toEqual([a.id]);
    const off = withBookmark(on, a.id, false);
    expect(off.topics[0]!.bookmarked).toBe(false);
    expect(off.bookmarked).toEqual([]);
  });

  it("dates cards like a phone's message list", () => {
    const now = 1_000_000;
    expect(when(now - 30, now)).toBe("now");
    expect(when(now - 12 * 60, now)).toBe("12 min");
    expect(when(now - 3 * 3600, now)).toBe("3 h");
    expect(when(now - 2 * 86400, now)).not.toMatch(/\d+ (min|h)$/);
  });

  it("finds where a topic starts among loaded messages, across days", () => {
    const topic = t("2026-10-12:40", "2026-10-13:2");
    expect(inTopic("2026-10-12:41", topic)).toBe(true);
    expect(inTopic("2026-10-13:2", topic)).toBe(true);
    expect(inTopic("2026-10-13:3", topic)).toBe(false);
    expect(inTopic("2026-10-12:39", topic)).toBe(false);
    expect(inTopic("sep:2026-10-12:41", topic)).toBe(false);
    // its first line not loaded (the page holds the last 100): the first loaded one inside it
    expect(startOf(topic, ["2026-10-12:38", "2026-10-13:0", "2026-10-13:1"])).toBe("2026-10-13:0");
    expect(startOf(topic, ["2026-10-13:5"])).toBeNull();
  });

  it("picks a topic up again by its title", () => {
    expect(pickUp(t("2026-10-13:0", "2026-10-13:0", { title: "Lisbon trip" }))).toBe('Let\'s pick up "Lisbon trip" again.');
  });
});

describe("bridge: Horizontal History requests", () => {
  it("posts topics and bookmark requests and resolves from onLink", async () => {
    const posts: OutboundMessage[] = [];
    const bridge = createBridge((m) => posts.push(m));
    const listed = bridge.requestLink({ type: "topics" });
    const sent = posts[0]!;
    expect(sent.type).toBe("topics");
    bridge.native.onLink((sent as { id: string }).id, JSON.stringify({ topics: [], bookmarked: [] }));
    await expect(listed).resolves.toEqual({ topics: [], bookmarked: [] });

    const saved = bridge.requestLink({ type: "bookmark", topic: "2026-10-13:0", on: true });
    expect(posts[1]).toMatchObject({ type: "bookmark", topic: "2026-10-13:0", on: true });
    bridge.native.onError((posts[1] as { id: string }).id, "Eve is not reachable");
    await expect(saved).rejects.toThrow("Eve is not reachable");
  });

  it("hears the composer's history button", () => {
    const bridge = createBridge(() => {});
    let n = 0;
    const off = bridge.onToggleHistory(() => n++);
    bridge.native.toggleHistory();
    bridge.native.toggleHistory();
    off();
    bridge.native.toggleHistory();
    expect(n).toBe(2);
  });
});
