// A json-render card drawn to markup: the spec reaches the registry's
// components and their words reach the page.
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { JsonCard } from "../JsonCard";

const line = (path: string, value: unknown) => JSON.stringify({ op: "add", path, value });
const el = (key: string, type: string, props: unknown, children: string[] = [], more: object = {}) =>
  line(`/elements/${key}`, { type, props, children, ...more });
const CARD = [
  line("/root", "card"),
  el("card", "Card", { title: "Train or plane", subtitle: "Door to door" }, ["table", "facts", "steps", "tags", "choose", "next"]),
  el("table", "Table", { columns: ["Compare", "Train"], rows: [["Time", "4 h 10 min"], ["Cost", 89]] }),
  el("facts", "Facts", { rows: [{ label: "Cheapest", value: "Train" }] }),
  el("steps", "Steps", { items: [{ title: "Book early", details: "Fares rise in the last week." }] }),
  el("tags", "Tags", { tags: ["Friday"] }),
  el("choose", "Buttons", {}, ["b1"]),
  el("b1", "Button", { label: "Book the train" }, [], { on: { press: { action: "ask", params: { message: "Book the train" } } } }),
  el("next", "FollowUps", { items: ["Find train times for Friday"] }),
].join("\n");

function draw(code: string, streaming = false): string {
  return renderToStaticMarkup(createElement(JsonCard, { stateKey: "m:0", code, streaming, onAsk: vi.fn(), onOpen: vi.fn() }));
}

describe("a json-render card", () => {
  it("draws every part of a finished block", () => {
    const html = draw(CARD);
    expect(html).toContain('data-engine="json-render"');
    for (const words of ["Train or plane", "Door to door", "Compare", "4 h 10 min", "89", "Cheapest", "Book early",
      "Fares rise in the last week.", "Friday", "Book the train", "Find train times for Friday"]) {
      expect(html).toContain(words);
    }
    expect(html).toContain("Show text");
    expect(html).not.toContain("couldn’t be displayed");
  });

  it("says so, and shows the words it could read, when a finished block is not a whole card", () => {
    const html = draw(CARD.replace('"type":"Table"', '"type":"Grid"'));
    expect(html).toContain("This card couldn’t be displayed. Recovered text may be incomplete.");
    expect(html).toContain("Book early");
  });

  it("waits with a status line while nothing drawable has arrived", () => {
    expect(draw('{"op":"add","path":"/ro', true)).toContain("Building your card…");
  });

  it("offers Reset inputs only on a card with controls", () => {
    const form = [
      line("/root", "card"), el("card", "Card", {}, ["city"]),
      el("city", "TextInput", { label: "City", placeholder: "Lisbon", value: { $bindState: "/form/city" } }),
      line("/state/form", { city: "Porto" }),
    ].join("\n");
    const html = draw(form);
    expect(html).toContain("Reset inputs");
    expect(html).toContain('value="Porto"');                      // the block's own state reached the input
    expect(draw(CARD)).not.toContain("Reset inputs");
  });
});

describe("a picture list where some items have no picture", () => {
  const list = (items: unknown[]) => [
    line("/root", "card"),
    el("card", "Card", { title: "Concerts" }, ["shows"]),
    el("shows", "List", { variant: "image", items }),
  ].join("\n");
  const pic = (n: number) => ({ src: `https://example.com/${n}.jpg`, alt: `Show ${n}` });

  it("fills the gaps with a gradient when at least half have a picture", () => {
    const html = draw(list([{ title: "A", image: pic(1) }, { title: "B", image: pic(2) }, { title: "C" }]));
    expect(html.match(/<img/g)?.length).toBe(3);
    expect(html).toContain("data:image/svg+xml");
    expect(html).toContain("https://example.com/1.jpg");
  });

  it("draws a numbered list when fewer than half have a picture", () => {
    const html = draw(list([{ title: "A", image: pic(1) }, { title: "B" }, { title: "C" }]));
    expect(html).not.toContain("<img");
  });

  it("gives one title the same gradient every time, and two titles different ones", async () => {
    const { gradientFor } = await import("../jsonRegistry");
    expect(gradientFor("Jazz night")).toBe(gradientFor("Jazz night"));
    expect(gradientFor("Jazz night")).not.toBe(gradientFor("Rock night"));
  });
});
