// Writes bridge/src/eve_bridge/json_render_prompt.txt: the json-render
// reference the brain gets for a turn from the Eve Link app while the card
// engine is json-render (the bridge's EVE_CARD_ENGINE; docs/app-chat.md,
// "The card engine").
//
// When to write a block at all is not decided here: the bridge puts that
// rule, with one small example, ahead of this reference (link_chat.JSON_APP_RULE).
//
// The component and action lists are the library's own (catalog.prompt() over
// src/jsonCatalog.ts), so they cannot drift from what the page draws. The rest
// of the library's prompt is written for a page builder: it asks for sample
// data, repeat lists over state and visibility rules, and says to use no code
// fence. eve answers from facts it has, on a phone, inside a fence, so the
// format, the rules and the examples here are eve's. Every example is checked
// with the page's own validator before anything is written.
//
// Run: pnpm gen-json-prompt            (writes the file)
//      pnpm check-json-prompt          (exits 1 when the committed file differs)
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { catalog, SPEC_FENCE } from "../src/jsonCatalog.ts";
import { inspectSpec } from "../src/specProgram.ts";

const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(here, "../../../../bridge/src/eve_bridge/json_render_prompt.txt");

const library = catalog.prompt({ mode: "standalone" });
function section(from, to) {
  const a = library.indexOf(from);
  const b = library.indexOf(to, a);
  if (a === -1 || b === -1) throw new Error(`the library's prompt has no "${from}" … "${to}" section any more`);
  return library.slice(library.indexOf("\n", a) + 1, b).trim();
}
const components = section("AVAILABLE COMPONENTS (", "AVAILABLE ACTIONS:");
const actions = section("AVAILABLE ACTIONS:", "EVENTS (the `on` field)").split("\n")
  .filter((line) => /^- (ask|open_url|setState):/.test(line));
if (actions.length !== 3) throw new Error("the library's action list changed; update the filter above");
if (components.split("\n").filter(Boolean).length !== catalog.componentNames.length) {
  throw new Error("the library's component list is not one line per component any more");
}
for (const name of catalog.componentNames) {
  if (!components.includes(`- ${name}:`)) throw new Error(`${name} is missing from the library's component list`);
}

const FORMAT = [
  "Inside the fence, write one JSON object per line and nothing else. Each line is an RFC 6902 JSON Patch \"add\" operation that builds the card:",
  "1. The first line names the root element: {\"op\":\"add\",\"path\":\"/root\",\"value\":\"card\"}",
  "2. Every other line adds one element under its own key: {\"op\":\"add\",\"path\":\"/elements/<key>\",\"value\":{\"type\":\"<Component>\",\"props\":{...},\"children\":[...]}}",
  "3. The root element is a Card. Its children are the keys of the elements it holds, top to bottom.",
  "4. Every element has type, props and children. children is a list of element keys; an element that holds nothing has \"children\": [].",
  "5. Every key in a children list is an element you add on a line of its own. Add the parent first, then its children in order, so the card fills in as it arrives.",
  "6. Every element except the root is named in exactly one children list. The card shows what its children lists hold, in their order.",
  "7. Each line is complete, valid JSON on one line: double quotes, no trailing commas, no comments.",
  "8. After the last line, close the fence with ``` on a line of its own.",
];

const EVENTS = [
  "A Button does something through its on field, a sibling of type, props and children (never inside props): \"on\":{\"press\":{\"action\":\"ask\",\"params\":{\"message\":\"Give me the taco recipe\"}}}.",
  "The message of ask is what the owner would have typed: write it as their words.",
];

const FORMS = [
  "A form keeps what the owner types or picks in the card's state. Add the starting values on a line of their own: {\"op\":\"add\",\"path\":\"/state/form\",\"value\":{\"city\":\"\"}}.",
  "Bind each control's value to its place in the state with a JSON Pointer path: \"value\":{\"$bindState\":\"/form/city\"}.",
  "The button that sends the form passes the values along: \"params\":{\"message\":\"Find hotels\",\"data\":{\"$state\":\"/form\"}}.",
];

const RULES = [
  "Use only the components and actions listed here.",
  "Use only facts you have from the conversation, a tool result or your own knowledge; never invent numbers to fill a table or chart.",
  "Keep it phone-sized: one Card, a few children, short labels, and at most about eight rows, items or bars.",
  "BarChart compares a few categories, LineChart shows a trend over time, PieChart shows parts of one whole; put the unit in the axis label or the series name.",
  "For a few choices the owner can answer with a tap, use Buttons with one Button per choice, each with an ask action, or FollowUps for likely next questions.",
  "Show a picture whenever one is available and the subject has a look: a concert or show, a place, venue, city, artist, team, dish, product or thing. An event from the event search carries its own image; for anything else, look one up with the picture tool by its proper name before writing the card. One subject: lead the Card with an Image. Several: a List with variant \"image\" and each item's image.",
  "Image needs a real image URL from a tool result, the conversation or the owner; with no such URL, leave images out.",
  "News articles and books from a tool get a List with variant \"image\": one item per article or book, 5 to 8 of them, with the headline or title, a short subtitle (an article: \"Section · date\"; a book: \"author · weeks on the list\"), the item's own image when it has one, and the item's link as its url so a tap opens it. Every news or book list ends with FollowUps: two or three short ways to keep browsing, such as more on the topic, another section or the most read.",
  "Callout is for something the owner must not miss (a warning, a deadline), never to explain what you did.",
];

const block = (lines) => `${SPEC_FENCE}\n${lines.map((l) => JSON.stringify(l)).join("\n")}\n\`\`\``;
const add = (path, value) => ({ op: "add", path, value });
const el = (key, type, props, children = [], more = {}) => add(`/elements/${key}`, { type, props, children, ...more });
const ask = (message, data) => ({ press: { action: "ask", params: data ? { message, data } : { message } } });

const EXAMPLES = [
  ["Steps (how to do something):", [
    add("/root", "card"),
    el("card", "Card", { title: "Descale the kettle", subtitle: "About 30 minutes" }, ["steps"]),
    el("steps", "Steps", { items: [
      { title: "Fill halfway", details: "Equal parts water and white vinegar." },
      { title: "Boil and wait", details: "Boil once, then let it stand for 20 minutes." },
      { title: "Rinse", details: "Empty it, then boil plain water twice and pour it away." },
    ] }),
  ]],
  ["Options with tappable choices:", [
    add("/root", "card"),
    el("card", "Card", { title: "Three quick dinners", subtitle: "Under 30 minutes" }, ["list", "choose"]),
    el("list", "List", { variant: "number", items: [
      { title: "Egg fried rice", subtitle: "Leftover rice, eggs, frozen peas" },
      { title: "Pasta aglio e olio", subtitle: "Spaghetti, garlic, chili, olive oil" },
      { title: "Black bean tacos", subtitle: "Tortillas, beans, salsa, cheese" },
    ] }),
    el("choose", "Buttons", { direction: "row" }, ["b1", "b2", "b3"]),
    el("b1", "Button", { label: "Fried rice", variant: "secondary" }, [], { on: ask("Give me the fried rice recipe") }),
    el("b2", "Button", { label: "Pasta", variant: "secondary" }, [], { on: ask("Give me the pasta recipe") }),
    el("b3", "Button", { label: "Tacos", variant: "secondary" }, [], { on: ask("Give me the taco recipe") }),
  ]],
  ["A chart from numbers the owner gave:", [
    add("/root", "card"),
    el("card", "Card", { title: "Your runs this week", subtitle: "Kilometres per day" }, ["chart", "totals"]),
    el("chart", "BarChart", { labels: ["Mon", "Tue", "Wed", "Thu", "Fri"], series: [{ name: "Distance", values: [5, 0, 7.5, 3, 10] }], xLabel: "Day", yLabel: "km" }),
    el("totals", "Facts", { rows: [{ label: "Total", value: "25.5 km" }, { label: "Longest", value: "Fri, 10 km" }] }),
  ]],
  ["News from a tool, each item opening its article:", [
    add("/root", "card"),
    el("card", "Card", { title: "Mars rover news", subtitle: "The New York Times" }, ["news", "next"]),
    el("news", "List", { variant: "image", items: [
      { title: "Rover Finds Signs of an Ancient Lake", subtitle: "Science · Oct 14", image: { src: "https://example.com/images/rover-lake.jpg", alt: "The rover on a ridge" }, url: "https://example.com/2026/10/14/rover-lake.html" },
      { title: "What the Next Rover Will Carry", subtitle: "Science · Oct 9", url: "https://example.com/2026/10/09/next-rover.html" },
    ] }),
    el("next", "FollowUps", { items: ["More on Mars", "Most read this week"] }),
  ]],
  ["A comparison with follow-ups:", [
    add("/root", "card"),
    el("card", "Card", { title: "Train or plane", subtitle: "Door to door" }, ["table", "next"]),
    el("table", "Table", { columns: ["Compare", "Train", "Plane"], rows: [["Time", "4 h 10 min", "3 h 30 min"], ["Cost", "$89", "$140"], ["Luggage", "Two bags", "One carry-on"]] }),
    el("next", "FollowUps", { items: ["Which is better for the climate?", "Find train times for Friday"] }),
  ]],
  ["A short form the owner fills in:", [
    add("/root", "card"),
    el("card", "Card", { title: "Find a hotel" }, ["city", "nights", "go"]),
    el("city", "TextInput", { label: "City", placeholder: "Lisbon", value: { $bindState: "/form/city" } }),
    el("nights", "Select", { label: "Nights", options: ["1", "2", "3", "4"], value: { $bindState: "/form/nights" } }),
    add("/state/form", { city: "", nights: "2" }),
    el("go", "Buttons", {}, ["search"]),
    el("search", "Button", { label: "Search" }, [], { on: ask("Find hotels", { $state: "/form" }) }),
  ]],
];
for (const [title, lines] of EXAMPLES) {
  const result = inspectSpec(lines.map((l) => JSON.stringify(l)).join("\n"));
  if (!result.complete) throw new Error(`example "${title}" does not validate: ${result.issues.join("; ")}`);
}

const numbered = (lines) => lines.map((l) => (/^\d+\. /.test(l) ? l : `- ${l}`)).join("\n");
const prompt = [
  "## json-render reference",
  `This is the reference for the json-render block in an Eve Link reply. Inside the ${SPEC_FENCE} fence the block follows every rule below; text outside the fence is shown as Markdown.`,
  "### Format",
  `${FORMAT[0]}\n${FORMAT.slice(1).join("\n")}`,
  "### Components",
  `A prop marked with ? is optional. A component marked [accepts children] holds other elements through its children list; every other component takes \"children\": [].\n\n${components}`,
  "### Actions",
  `${actions.join("\n")}\n\n${numbered(EVENTS)}`,
  "### Forms",
  numbered(FORMS),
  "### Rules",
  numbered(RULES),
  "### Examples",
  EXAMPLES.map(([title, lines]) => `${title}\n\n${block(lines)}`).join("\n\n"),
].join("\n\n") + "\n";

if (process.argv.includes("--check")) {
  let committed = "";
  try { committed = readFileSync(out, "utf8"); } catch { /* absent: stale */ }
  if (committed !== prompt) {
    console.error(`${out} is stale: run pnpm gen-json-prompt`);
    process.exit(1);
  }
  console.log(`up to date: ${prompt.length} characters, ${catalog.componentNames.length} components, ${EXAMPLES.length} examples`);
} else {
  mkdirSync(dirname(out), { recursive: true });
  writeFileSync(out, prompt);
  console.log(`wrote ${out}: ${prompt.length} characters, ${catalog.componentNames.length} components, ${EXAMPLES.length} examples`);
}
