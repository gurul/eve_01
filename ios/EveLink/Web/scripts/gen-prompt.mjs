// Writes bridge/src/eve_bridge/openui_prompt.txt: the OpenUI Lang
// reference the brain gets for a turn that came from the Eve Link app.
//
// When to use OpenUI at all is not decided here: the bridge puts that rule,
// with one small example, ahead of this reference (link_chat.APP_RULE).
//
// The reference is the library's own prompt (generateSystemPrompt over
// openuiChatLibrary's spec), trimmed to the components that suit a reply in a
// phone chat (CHAT_COMPONENTS). The full library prompt was about 54,000
// characters, mostly signatures, notes and five large examples for desktop
// dashboards; eve's replies on a phone need a small part of it. Trimming:
//
// - only CHAT_COMPONENTS get a signature, and every type in a kept signature
//   that names a left-out component is dropped from it (Card's children, a
//   TabItem's content, a FormControl's input), so the reference names only
//   components it describes;
// - a group note or library rule that names a left-out component is dropped;
//   so is one that does not fit eve (DROP_RULES), and RULES are added;
// - the library's examples are replaced by EXAMPLES, small phone-sized
//   programs, each checked with the app's parser before anything is written.
//
// The app still renders every component of the library (src/AssistantMessage.tsx
// uses openuiChatLibrary whole); the trim only decides what the brain is told.
//
// Run: pnpm gen-prompt            (writes the file)
//      pnpm gen-prompt --check    (exits 1 when the committed file differs)

// lang-core's telemetry is opt-in at runtime; make sure it stays off here.
process.env.OPENUI_TELEMETRY_DISABLED = "1";
process.env.DO_NOT_TRACK = "1";

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const { openuiChatLibrary } = await import("@openuidev/react-ui/genui-lib");
const { openuiChatPromptOptions } = await import("@openuidev/react-ui/genui-lib");
const { createParser, generateSystemPrompt } = await import("@openuidev/react-lang");

const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(here, "../../../../bridge/src/eve_bridge/openui_prompt.txt");

/** The components the brain is told about: what a reply in a phone chat uses. */
export const CHAT_COMPONENTS = [
  "Card", "CardHeader", "TextContent", "Callout",
  "Table", "Col", "EntityList",
  "ListBlock", "ListItem", "Steps", "StepsItem",
  "BarChart", "LineChart", "PieChart", "Series",
  "Buttons", "Button", "FollowUpBlock", "FollowUpItem",
  "Tag", "TagBlock", "Icon",
  "Tabs", "TabItem", "Accordion", "AccordionItem",
  "ImageBlock",
  "Form", "FormControl", "Input", "Select", "SelectItem", "Chips", "ChipItem",
];

const PREAMBLE =
  "## openui-lang reference\n\n" +
  "This is the reference for the openui-lang block in an Eve Link reply. Inside the ```openui-lang fence the " +
  "program follows every rule below; text outside the fence is shown as Markdown.";

// Library notes and rules to leave out, matched by a phrase in them. Some do not fit eve: it answers from facts it
// has, so it never makes data up; a phone reply is small, so a chart is not required on every number; and a
// templated image host is still an invented URL. The rest belong to left-out components without naming them (the
// editable table's rows, image carousels, card blocks).
const DROP_RULES = [
  "realistic/plausible data", "Quantitative asks get a chart", "picsum.photos",
  "Save Changes", "image carousels", "card-block",
];

const RULES = [
  "Use only facts you have from the conversation, a tool result or your own knowledge; never invent numbers to fill a table or chart.",
  "Keep it phone-sized: one Card, a few children, short labels, and at most about eight rows, items or bars.",
  "BarChart compares a few categories, LineChart shows a trend over time, PieChart shows parts of one whole; put the unit in the axis label or the series name.",
  "For a few choices the owner can answer with a tap, use Buttons with one Button per choice (each sends its label back to you), or a FollowUpBlock for likely next questions.",
  "ImageBlock needs a real image URL from a tool result, the conversation or the owner; with no such URL, leave images out.",
  "Callout is for something the owner must not miss (a warning, a deadline), never to explain what you did.",
];

const EXAMPLES = [
  `Steps (how to do something):

\`\`\`openui-lang
root = Card([header, steps])
header = CardHeader("Descale the kettle", "About 30 minutes")
steps = Steps([s1, s2, s3])
s1 = StepsItem("Fill halfway", "Equal parts water and white vinegar.")
s2 = StepsItem("Boil and wait", "Boil once, then let it stand for 20 minutes.")
s3 = StepsItem("Rinse", "Empty it, then boil plain water twice and pour it away.")
\`\`\``,
  `Options with tappable choices:

\`\`\`openui-lang
root = Card([header, list, choose])
header = CardHeader("Three quick dinners", "Under 30 minutes")
list = ListBlock([d1, d2, d3], "number")
d1 = ListItem("Egg fried rice", "Leftover rice, eggs, frozen peas")
d2 = ListItem("Pasta aglio e olio", "Spaghetti, garlic, chili, olive oil")
d3 = ListItem("Black bean tacos", "Tortillas, beans, salsa, cheese")
choose = Buttons([b1, b2, b3], "row")
b1 = Button("Fried rice", @ToAssistant("Give me the fried rice recipe"), "secondary")
b2 = Button("Pasta", @ToAssistant("Give me the pasta recipe"), "secondary")
b3 = Button("Tacos", @ToAssistant("Give me the taco recipe"), "secondary")
\`\`\``,
  `A chart from numbers the owner gave:

\`\`\`openui-lang
root = Card([header, chart, totals])
header = CardHeader("Your runs this week", "Kilometres per day")
chart = BarChart(["Mon", "Tue", "Wed", "Thu", "Fri"], [km], "grouped", "Day", "km")
km = Series("Distance", [5, 0, 7.5, 3, 10])
totals = EntityList([{ left: "Total", right: "25.5 km", rightVariant: "number" }, { left: "Longest", right: "Fri, 10 km", rightVariant: "text" }])
\`\`\``,
  `A comparison with follow-ups:

\`\`\`openui-lang
root = Card([header, table, next])
header = CardHeader("Train or plane", "Door to door")
table = Table([Col("Compare", ["Time", "Cost", "Luggage"]), Col("Train", ["4 h 10 min", "$89", "Two bags"]), Col("Plane", ["3 h 30 min", "$140", "One carry-on"])])
next = FollowUpBlock([f1, f2])
f1 = FollowUpItem("Which is better for the climate?")
f2 = FollowUpItem("Find train times for Friday")
\`\`\``,
];

const spec = openuiChatLibrary.toSpec();
const known = Object.keys(spec.components);
const keep = new Set(CHAT_COMPONENTS);
const left = new Set(known.filter((n) => !keep.has(n)));
for (const name of CHAT_COMPONENTS) {
  if (!known.includes(name)) throw new Error(`${name} is not in openuiChatLibrary any more; update CHAT_COMPONENTS`);
}

const word = (name) => new RegExp(`\\b${name}s?\\b`);   // "ContextCardItems" names one too
const namesLeftOut = (text) => [...left].filter((n) => word(n).test(text));

/** A signature with every left-out component dropped from its type unions. */
function trimSignature(sig) {
  const trimmed = sig.replace(/\b[A-Z][A-Za-z0-9]*(?: \| [A-Z][A-Za-z0-9]*)+\b/g, (union) => {
    const members = union.split(" | ").filter((m) => !left.has(m));
    if (!members.length) throw new Error(`every member of ${union} is left out`);
    return members.join(" | ");
  });
  return trimmed.replace(/\(([A-Z][A-Za-z0-9]*)\)\[\]/g, "$1[]");
}

const components = {};
for (const name of CHAT_COMPONENTS) {
  const c = spec.components[name];
  components[name] = { ...c, signature: trimSignature(c.signature) };
}
const componentGroups = spec.componentGroups
  .map((g) => ({
    ...g,
    components: g.components.filter((n) => keep.has(n)),
    notes: (g.notes ?? []).filter((note) => !namesLeftOut(note).length && !DROP_RULES.some((d) => note.includes(d))),
  }))
  .filter((g) => g.components.length);

const additionalRules = [
  ...(openuiChatPromptOptions.additionalRules ?? []).filter(
    (r) => !namesLeftOut(r).length && !DROP_RULES.some((d) => r.includes(d)),
  ),
  ...RULES,
];

let prompt = generateSystemPrompt({
  library: { root: spec.root, components, componentGroups },
  promptOptions: { preamble: PREAMBLE, examples: EXAMPLES, additionalRules },
}).trim();

// Some of the library's text names things a program cannot write. Two syntax rules show a placeholder component
// name, and one note uses an action name it never defines; show real components and a defined action instead. The
// ListItem note, its example and the Buttons rule speak of "the LLM" and "the user"; name the owner instead, and write
// the example as a program.
const REWRITES = [
  [/component calls TypeName\(arg1, arg2, \.\.\.\)/, 'component calls such as CardHeader("Title", "Subtitle")'],
  [
    /`SomeComp\(\[children\], "row", "l"\)` NOT `SomeComp\(\[children\], direction: "row", gap: "l"\)`/,
    '`Buttons([yes, no], "row")` NOT `Buttons([yes, no], direction: "row")`',
  ],
  [
    /Clicking a FollowUpItem, or a ListItem with a @ToAssistant action, sends text to the LLM as a user message\./,
    'Tapping a FollowUpItem sends its text to you as the owner\'s next message; so does tapping a ListItem whose action is @ToAssistant("...").',
  ],
  [
    /Example: list = ListBlock\(\[item1, item2\]\) {2}item1 = ListItem\("Option A", "Details about A", null, null, @ToAssistant\("Option A"\)\)/,
    'Example: `list = ListBlock([item1, item2])` with `item1 = ListItem("Option A", "Details about A", null, null, @ToAssistant("Tell me more about Option A"))`',
  ],
  [
    /Button\("Go", onSubmit\) and Button\("Go", Action\(\[\.\.\.\]\)\) both work/,
    'go = @ToAssistant("Go") then Button("Go", go), or Button("Go", @ToAssistant("Go")); both work',
  ],
  [
    /a URL from a tool or the user \(never fabricated\), or a @ToAssistant follow-up\. Never add/,
    "a URL from a tool or the owner (never made up), or an @ToAssistant follow-up. Never add",
  ],
];
for (const [from, to] of REWRITES) {
  const before = prompt;
  prompt = prompt.replace(from, to);
  if (prompt === before) throw new Error(`a syntax rule changed upstream (${from}); update its rewrite`);
}

// Every component the reference calls or types is one it describes, and nothing outside the library is named.
const parser = createParser(openuiChatLibrary.toJSONSchema(), openuiChatLibrary.root);
// (an @-prefixed name is an action step, such as @ToAssistant, not a component)
const called = new Set([...prompt.matchAll(/(?<![@\w])([A-Z][A-Za-z0-9]*)\(/g)].map((m) => m[1]));
for (const name of called) {
  if (name !== "Action" && !keep.has(name)) throw new Error(`the reference calls ${name}(), which it does not describe`);
}
const leaked = namesLeftOut(prompt).filter((n) => /[a-z][A-Z]/.test(n) || new RegExp(`(: |\\| |\\()${n}\\b|\\b${n}( \\||\\[\\])`).test(prompt));
if (leaked.length) throw new Error(`the reference still names left-out components: ${leaked.join(", ")}`);
if (/\bStack\b/.test(prompt)) throw new Error("the reference names a layout the library does not have");
// The action steps it names are the ones the Action section lists, and no action is written another way.
const STEPS = new Set(["ToAssistant", "OpenUrl"]);
for (const [, step] of prompt.matchAll(/@([A-Za-z]+)\(/g)) {
  if (!STEPS.has(step)) throw new Error(`the reference names an action step that does not exist: @${step}`);
}
for (const step of STEPS) {
  if (!prompt.includes(`- @${step}(`)) throw new Error(`the Action section no longer lists @${step}; update STEPS`);
}
const snake = [...new Set(prompt.match(/\b[a-z]+_[a-z_]+\b/g) ?? [])];
if (snake.length) throw new Error(`the reference names identifiers no program can write: ${snake.join(", ")}`);
if (/\{\s*type:/.test(prompt)) throw new Error("the reference writes an action as an object; use @Step(...), or Action([...]) for several");
// The inline example the ListBlock note gives parses, as the examples do.
const inline = 'root = Card([list])\nlist = ListBlock([item1, item2])\nitem1 = ListItem("Option A", "Details about A", null, null, @ToAssistant("Tell me more about Option A"))\nitem2 = ListItem("Option B")';
if (!prompt.includes(inline.split("\n")[2])) throw new Error("the ListBlock note's example changed; update its check");
{
  const r = parser.parse(inline);
  const bad = [...r.meta.errors.map((e) => e.message), ...r.meta.unresolved, ...r.meta.orphaned];
  if (!r.root || r.meta.incomplete || bad.length) throw new Error(`the ListBlock note's example does not parse: ${bad.join("; ")}`);
}
for (const example of EXAMPLES) {
  const code = example.split("```openui-lang\n")[1].split("\n```")[0];
  const r = parser.parse(code);
  const bad = [...r.meta.errors.map((e) => e.message), ...r.meta.unresolved, ...r.meta.orphaned];
  if (!r.root || r.meta.incomplete || bad.length) throw new Error(`an example does not parse cleanly: ${bad.join("; ")}`);
}

const text = `${prompt}\n`;
if (process.argv.includes("--check")) {
  const committed = readFileSync(out, "utf8");
  if (committed !== text) {
    console.error(`${out} is stale: run pnpm gen-prompt`);
    process.exit(1);
  }
  console.log(`up to date (${prompt.length} chars, ${CHAT_COMPONENTS.length} components)`);
} else {
  mkdirSync(dirname(out), { recursive: true });
  writeFileSync(out, text);
  console.log(`wrote ${out} (${prompt.length} chars, ${CHAT_COMPONENTS.length} components)`);
}
