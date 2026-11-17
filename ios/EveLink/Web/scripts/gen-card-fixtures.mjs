// Writes ios/EveLink/Packages/EveCards/Tests/EveCardCoreTests/Fixtures/card-parity.json:
// a set of json-render blocks with what the web page's checker (src/specProgram.ts)
// says about each. The native card engine's tests run the same blocks through
// its own checker (EveCardCore, inspectCard) and must agree, so the two cannot
// drift (docs/app-chat.md, "The card engine").
//
// The cases: the six example blocks the brain is shown
// (bridge/src/eve_bridge/json_render_prompt.txt), one block that uses every
// component, the shapes a block may take, and the ways one goes wrong.
// Everything in them is made up.
//
// Run: pnpm gen-card-fixtures           (writes the file)
//      pnpm check-card-fixtures         (exits 1 when the committed file differs)
// Node 22.18 or later (it runs the TypeScript sources as they are).
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { catalog, SPEC_FENCE } from "../src/jsonCatalog.ts";
import { compileSpec, inspectSpec } from "../src/specProgram.ts";

const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(here, "../../Packages/EveCards/Tests/EveCardCoreTests/Fixtures/card-parity.json");
const promptFile = resolve(here, "../../../../bridge/src/eve_bridge/json_render_prompt.txt");

const add = (path, value) => JSON.stringify({ op: "add", path, value });
const el = (key, type, props, children = [], more = {}) => add(`/elements/${key}`, { type, props, children, ...more });
const ask = (message, data) => ({ press: { action: "ask", params: data ? { message, data } : { message } } });
const block = (...lines) => lines.join("\n");

/** A block that uses every component in the catalog. The native tests draw it too. */
export const EVERY_COMPONENT = block(
  add("/root", "card"),
  el("card", "Card", { title: "A weekend in Porto", subtitle: "Two days, on foot" },
    ["intro", "note", "picture", "tags", "facts", "tabs", "places", "steps", "more", "bars", "line", "pie", "form", "choose", "next"]),
  el("intro", "Text", { text: "A **short** plan for two days by the river, with *one* hill." }),
  el("note", "Callout", { variant: "warning", title: "Book the cellar tour", description: "Tours fill up by **Thursday**." }),
  el("picture", "Image", { src: "https://example.com/images/river.jpg", alt: "A river with a two-level iron bridge" }),
  el("tags", "Tags", { tags: ["Walkable", "Food", "River", "Under $300"] }),
  el("facts", "Facts", { rows: [{ label: "Train from Lisbon", value: "2 h 50 min" }, { label: "Budget", value: "$280" }] }),
  el("tabs", "Tabs", { tabs: ["Saturday", "Sunday"] }, ["sat", "sun"]),
  el("sat", "Stack", { direction: "column" }, ["satText", "satTable"]),
  el("satText", "Text", { text: "Old town in the morning, the river after lunch." }),
  el("satTable", "Table", { columns: ["Time", "Place", "Cost"], rows: [["9:00", "Market hall", 0], ["11:30", "Tile museum", 8.5], ["15:00", "River walk", 0]] }),
  el("sun", "Stack", { direction: "row" }, ["sunText", "sunTags"]),
  el("sunText", "Text", { text: "Across the bridge for the cellars." }),
  el("sunTags", "Tags", { tags: ["Slow start"] }),
  el("places", "List", { variant: "image", items: [
    { title: "Market hall", subtitle: "Breakfast · opens 8:00", image: { src: "https://example.com/images/market.jpg", alt: "Stalls under a glass roof" }, url: "https://example.com/places/market" },
    { title: "Tile museum", subtitle: "Closed Mondays", image: { src: "https://example.com/images/tiles.jpg", alt: "Blue and white tiles" } },
    { title: "River walk", subtitle: "Flat, 3 km" },
  ] }),
  el("steps", "Steps", { items: [
    { title: "Buy a day ticket", details: "At any metro machine; it covers the **funicular**." },
    { title: "Start at the top", details: "Walk downhill to the river." },
    { title: "Cross on the upper deck" },
  ] }),
  el("more", "Accordion", { sections: ["What to pack", "If it rains"] }, ["pack", "rain"]),
  el("pack", "List", { variant: "number", items: [{ title: "Flat shoes", subtitle: "The streets are cobbled" }, { title: "A light jacket" }] }),
  el("rain", "Text", { text: "Swap the river walk for the tile museum." }),
  el("bars", "BarChart", { labels: ["Food", "Sights", "Transit"], series: [{ name: "Saturday", values: [60, 25, 12] }, { name: "Sunday", values: [45, 40, 8] }], variant: "grouped", xLabel: "Category", yLabel: "USD" }),
  el("line", "LineChart", { labels: ["8:00", "12:00", "16:00", "20:00"], series: [{ name: "Temperature", values: [14, 19.5, 21, 16] }], xLabel: "Time", yLabel: "°C" }),
  el("pie", "PieChart", { labels: ["Food", "Sights", "Transit"], values: [105, 65, 20], variant: "donut" }),
  el("form", "Stack", {}, ["name", "pace", "likes"]),
  el("name", "TextInput", { label: "Travelling from", placeholder: "Lisbon", value: { $bindState: "/form/from" } }),
  el("pace", "Select", { label: "Pace", options: ["Slow", "Steady", "Fast"], value: { $bindState: "/form/pace" } }),
  el("likes", "Chips", { label: "Interests", options: ["Food", "Museums", "Views", "Music"], value: { $bindState: "/form/likes" } }),
  add("/state/form", { from: "", pace: "Steady", likes: ["Food"] }),
  el("choose", "Buttons", { direction: "row" }, ["plan", "map"]),
  el("plan", "Button", { label: "Plan it" }, [], { on: ask("Plan my Porto weekend", { $state: "/form" }) }),
  el("map", "Button", { label: "Open the map", variant: "secondary" }, [], { on: { press: { action: "open_url", params: { url: "https://example.com/maps/porto" } } } }),
  el("next", "FollowUps", { items: ["Where should we eat on Saturday?", "Make it three days"] }),
);

const GOOD = block(
  add("/root", "card"),
  el("card", "Card", { title: "Dinner", subtitle: "For two" }, ["text", "table"]),
  el("text", "Text", { text: "Roast vegetables" }),
  el("table", "Table", { columns: ["Dish", "Minutes"], rows: [["Soup", 20], ["Tart", 45]] }),
);

/** The example blocks in the reference the brain reads, in order. */
function promptExamples() {
  const text = readFileSync(promptFile, "utf8");
  const blocks = [];
  const fence = `${SPEC_FENCE}\n`;
  for (let at = text.indexOf(fence); at !== -1; at = text.indexOf(fence, at + 1)) {
    const start = at + fence.length;
    blocks.push(text.slice(start, text.indexOf("\n```", start)));
  }
  if (blocks.length !== 6) throw new Error(`expected six example blocks in ${promptFile}, found ${blocks.length}`);
  return blocks;
}

/** [name, code, streaming] */
const CASES = [
  ...promptExamples().map((code, i) => [`prompt example ${i + 1}`, code, false]),
  ["every component", EVERY_COMPONENT, false],
  ["every component, streaming", EVERY_COMPONENT, true],
  ["a whole JSON spec", JSON.stringify(compileSpec(GOOD).spec, null, 2), false],
  ["a whole JSON spec with state", JSON.stringify({ root: "c", elements: { c: { type: "Card", props: {}, children: ["t"] }, t: { type: "Text", props: { text: "Hi" }, children: [] } }, state: { n: 1 } }), false],
  ["patch lines", GOOD, false],
  ["a streaming block cut mid-line", GOOD.slice(0, GOOD.lastIndexOf("\n") + 40), true],
  ["the same cut block, finished", GOOD.slice(0, GOOD.lastIndexOf("\n") + 40), false],
  ["streaming, only half a first line", '{"op":"add","path":"/ro', true],
  ["streaming, a root and nothing else", add("/root", "card"), true],
  ["streaming, a child before its parent", block(add("/root", "card"), el("text", "Text", { text: "Early" })), true],
  ["an orphan element", block(GOOD, el("extra", "Text", { text: "One more thing" })), false],
  ["an orphan element, streaming", block(GOOD, el("extra", "Text", { text: "One more thing" })), true],
  ["orphans with number keys, shown smallest first", block(add("/root", "card"), el("card", "Card", {}, ["a"]), el("a", "Text", { text: "First" }), el("b", "Text", { text: "Named" }), el("10", "Text", { text: "Ten" }), el("2", "Text", { text: "Two" })), false],
  ["a leaf with no children list", block(add("/root", "c"), add("/elements/c", { type: "Card", props: {}, children: ["t"] }), add("/elements/t", { type: "Text", props: { text: "Hi" } })), false],
  ["windows line endings and blank lines", GOOD.split("\n").join("\r\n\r\n") + "\r\n", false],
  ["props set by a later patch", block(GOOD, add("/elements/card/props/title", "Supper"), add("/elements/card/children/-", "late"), el("late", "Text", { text: "Added late" })), false],
  ["replace, remove, move, copy and test", block(GOOD,
    JSON.stringify({ op: "replace", path: "/elements/text/props/text", value: "Roast roots" }),
    JSON.stringify({ op: "copy", from: "/elements/text", path: "/elements/again" }),
    JSON.stringify({ op: "add", path: "/elements/card/children/1", value: "again" }),
    JSON.stringify({ op: "move", from: "/elements/table", path: "/elements/grid" }),
    JSON.stringify({ op: "replace", path: "/elements/card/children/2", value: "grid" }),
    JSON.stringify({ op: "test", path: "/root", value: "card" }),
    JSON.stringify({ op: "remove", path: "/elements/card/props/subtitle" })), false],
  ["a test patch that fails", block(GOOD, JSON.stringify({ op: "test", path: "/root", value: "other" })), false],
  ["a patch through a null", block(GOOD, add("/state", null), add("/state/form/city", "x")), false],
  ["a patch with a path that is not a string", block(GOOD, JSON.stringify({ op: "add", path: 5, value: 1 })), false],
  ["an op the stream does not know", block(GOOD, JSON.stringify({ op: "merge", path: "/root", value: "x" })), false],
  ["numbers as text", block(add("/root", "c"), el("c", "Card", {}, ["t", "p"]),
    el("t", "Table", { columns: ["A", "B", "C", "D", "E", "F"], rows: [[0.1, 1e21, 1e-7, 100, 1.5, -2.25], [123456789012, 0.000001, 1e300, 5e-324, -0, 12345.678]] }),
    el("p", "PieChart", { labels: ["x", "y"], values: [33.3, 1e22] })), false],
  ["tabs and an accordion as text", block(add("/root", "c"), el("c", "Card", { title: "T" }, ["tabs", "acc"]),
    el("tabs", "Tabs", { tabs: ["One", "Two"] }, ["a", "b", "x"]), el("a", "Text", { text: "A" }), el("b", "Text", { text: "B" }), el("x", "Text", { text: "X" }),
    el("acc", "Accordion", { sections: ["S1"] }, ["d"]), el("d", "Facts", { rows: [{ label: "L", value: "V" }] })), false],
  ["a child named twice", block(add("/root", "c"), el("c", "Card", {}, ["t", "t"]), el("t", "Text", { text: "Once" })), false],
  ["state and no controls", block(GOOD, add("/state/count", 1)), false],
  ["a nullish prop set to null", block(add("/root", "c"), el("c", "Card", { title: null }, ["l"]), el("l", "List", { variant: null, items: [{ title: "A", subtitle: null, image: null, url: null }] })), false],
  ["on written inside props", block(add("/root", "c"), el("c", "Card", {}, ["b"]), el("b", "Button", { label: "Go", on: ask("Go") })), false],
  ["a $state value inside a table cell and an image", block(add("/root", "c"), el("c", "Card", {}, ["t", "l"]),
    el("t", "Table", { columns: ["A"], rows: [[{ $state: "/a" }]] }),
    el("l", "List", { items: [{ title: "A", image: { src: { $state: "/src" }, alt: "x" } }] }), add("/state", { a: "1", src: "https://example.com/a.jpg" })), false],
  ["a $bindState value where a string is expected", block(add("/root", "c"), el("c", "Card", { title: { $bindState: "/title" } }, ["t"]), el("t", "Text", { text: { $bindState: "/text" } }), add("/state", { title: "T", text: "Body" })), false],
  ["props that are one dynamic value", block(add("/root", "c"), el("c", "Card", {}, ["t"]), el("t", "Text", { $state: "/props" })), false],
  // the bad ones
  ["bad: no block at all", "", false],
  ["bad: not a patch line", `${GOOD}\nThat is the plan.`, false],
  ["bad: a JSON line that is not a patch", `${GOOD}\n{"note":"done"}`, false],
  ["bad: root not a Card", GOOD.replace('"type":"Card"', '"type":"Stack"'), false],
  ["bad: root with no children", block(add("/root", "card"), el("card", "Card", { title: "Empty" })), false],
  ["bad: root names an element that is not there", block(add("/root", "main"), el("card", "Card", {}, ["t"]), el("t", "Text", { text: "Hi" })), false],
  ["bad: unknown component", GOOD.replace('"Table"', '"Grid"'), false],
  ["bad: missing child", GOOD.replace('["text","table"]', '["text","table","gone"]'), false],
  ["bad: wrong prop type", GOOD.replace('[["Soup",20],["Tart",45]]', '"Soup, 20"'), false],
  ["bad: a number where a string is expected", GOOD.replace('"Roast vegetables"', "12"), false],
  ["bad: an enum value outside the set", block(add("/root", "c"), el("c", "Card", {}, ["k"]), el("k", "Callout", { variant: "danger", title: "Careful" })), false],
  ["bad: missing required prop", block(add("/root", "c"), el("c", "Card", {}, ["f"]), el("f", "Facts", { rows: [{ label: "Only a label" }] })), false],
  ["bad: a required prop set to null", block(add("/root", "c"), el("c", "Card", {}, ["t"]), el("t", "Text", { text: null })), false],
  ["bad: a list where chips take strings", block(add("/root", "c"), el("c", "Card", {}, ["k"]), el("k", "Chips", { options: ["a"], value: [1, 2] })), false],
  ["bad: an image that is not an object", block(add("/root", "c"), el("c", "Card", {}, ["l"]), el("l", "List", { items: [{ title: "A", image: "https://example.com/a.jpg" }] })), false],
  ["bad: an invalid visible condition", block(add("/root", "c"), el("c", "Card", {}, ["t"]), el("t", "Text", { text: "Hi" }, [], { visible: { $state: "/a", $item: "b" } })), false],
  ["bad: repeat with no children", block(add("/root", "c"), el("c", "Card", {}, ["t"]), el("t", "Text", { text: "Hi" }, [], { repeat: { statePath: "/rows" } }), add("/state/rows", [1])), false],
  ["bad: slots that are not lists", block(add("/root", "c"), el("c", "Card", {}, ["t"]), el("t", "Text", { text: "Hi" }, [], { slots: { header: "x" } })), false],
  ["bad: a cut-off stream read as finished", GOOD.slice(0, -30), false],
];

const names = new Set(CASES.map(([name]) => name));
if (names.size !== CASES.length) throw new Error("two cases share a name");
const used = new Set();
const cases = CASES.map(([name, code, streaming]) => {
  const r = inspectSpec(code, streaming);
  for (const e of Object.values(r.spec?.elements ?? {})) used.add(String(e.type));
  return {
    name, code, streaming,
    renderable: r.renderable, complete: r.complete, hasControls: r.hasControls,
    issueCount: r.issues.length, adopted: r.adopted, text: r.text,
    // what the renderer is given: the elements kept, and the root's children in order
    elements: r.spec ? Object.keys(r.spec.elements).sort() : null,
    rootChildren: r.spec ? (r.spec.elements[r.spec.root]?.children ?? null) : null,
  };
});

// The set must keep its meaning: the examples and the every-component block are whole cards, every component is
// drawn at least once, and every case marked bad fails.
for (const c of cases) {
  const wantsComplete = c.name.startsWith("prompt example") || c.name === "every component";
  if (wantsComplete && !c.complete) throw new Error(`"${c.name}" should be a complete card`);
  if (c.name.startsWith("bad:") && (c.complete || c.issueCount === 0)) throw new Error(`"${c.name}" should fail`);
}
const every = inspectSpec(EVERY_COMPONENT);
const drawn = new Set(Object.values(every.spec.elements).map((e) => e.type));
for (const name of catalog.componentNames) if (!drawn.has(name)) throw new Error(`the every-component block has no ${name}`);

const text = `${JSON.stringify({ cases }, null, 2)}\n`;
if (process.argv.includes("--check")) {
  let committed = "";
  try { committed = readFileSync(out, "utf8"); } catch { /* absent: stale */ }
  if (committed !== text) {
    console.error(`${out} is stale: run pnpm gen-card-fixtures`);
    process.exit(1);
  }
  console.log(`up to date: ${cases.length} cases`);
} else {
  mkdirSync(dirname(out), { recursive: true });
  writeFileSync(out, text);
  console.log(`wrote ${out}: ${cases.length} cases`);
}
