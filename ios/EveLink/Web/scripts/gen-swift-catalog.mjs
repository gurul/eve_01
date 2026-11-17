// Writes ios/EveLink/Packages/EveCards/Sources/EveCardCore/Resources/card-catalog.json:
// eve's json-render catalog (src/jsonCatalog.ts) as data the native card engine
// reads. The Swift validator checks a block's props against these schemas, so
// the native app and the web page cannot disagree about what a component takes
// (docs/app-chat.md, "The card engine").
//
// Each component's props, and each action's params, are zod's own JSON Schema
// export of the catalog's zod type. Nothing here is written by hand.
//
// Run: pnpm gen-swift-catalog           (writes the file)
//      pnpm check-swift-catalog         (exits 1 when the committed file differs)
// Node 22.18 or later (it runs the TypeScript sources as they are).
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { z } from "zod";
import { catalog, CONTROLS, ROOT } from "../src/jsonCatalog.ts";

const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(here, "../../Packages/EveCards/Sources/EveCardCore/Resources/card-catalog.json");

/** A zod type as JSON Schema, as the type reads its input (so an object takes keys it does not name). */
function jsonSchema(type) {
  const { $schema: _draft, ...schema } = z.toJSONSchema(type, { io: "input" });
  return schema;
}

const components = Object.entries(catalog.data.components).map(([name, def]) => ({
  name,
  props: jsonSchema(def.props),
  slots: def.slots ?? [],
  events: def.events ?? [],
}));
const actions = Object.entries(catalog.data.actions).map(([name, def]) => ({
  name,
  params: jsonSchema(def.params),
}));

// The Swift checker reads a fixed subset of JSON Schema. A keyword outside it
// would be ignored there, and the two validators would drift without a word.
const KEYWORDS = new Set(["type", "properties", "required", "items", "enum", "anyOf", "propertyNames", "additionalProperties"]);
const TYPES = new Set(["object", "string", "number", "array", "null"]);
function checkSubset(schema, at) {
  if (schema === null || typeof schema !== "object" || Array.isArray(schema)) throw new Error(`${at}: not a schema object`);
  for (const [key, value] of Object.entries(schema)) {
    if (!KEYWORDS.has(key)) throw new Error(`${at}: the Swift checker does not read "${key}"; teach it (PropSchema.swift) and add the keyword here`);
    if (key === "type") {
      for (const t of [].concat(value)) if (!TYPES.has(t)) throw new Error(`${at}: the Swift checker does not read type "${t}"`);
    }
    if (key === "properties") for (const [name, sub] of Object.entries(value)) checkSubset(sub, `${at}.${name}`);
    if (key === "items") checkSubset(value, `${at}[]`);
    if (key === "anyOf") value.forEach((sub, i) => checkSubset(sub, `${at}|${i}`));
    if (key === "additionalProperties" && JSON.stringify(value) !== "{}") throw new Error(`${at}: additionalProperties is read as "any value" only`);
    if (key === "propertyNames" && JSON.stringify(value) !== '{"type":"string"}') throw new Error(`${at}: propertyNames is read as "any string" only`);
  }
}
for (const c of components) checkSubset(c.props, c.name);
for (const a of actions) checkSubset(a.params, a.name);
if (!catalog.componentNames.includes(ROOT)) throw new Error(`the root component ${ROOT} is not in the catalog`);
for (const name of CONTROLS) if (!catalog.componentNames.includes(name)) throw new Error(`the control ${name} is not in the catalog`);

const text = `${JSON.stringify({ root: ROOT, controls: CONTROLS, components, actions }, null, 2)}\n`;

if (process.argv.includes("--check")) {
  let committed = "";
  try { committed = readFileSync(out, "utf8"); } catch { /* absent: stale */ }
  if (committed !== text) {
    console.error(`${out} is stale: run pnpm gen-swift-catalog`);
    process.exit(1);
  }
  console.log(`up to date: ${components.length} components, ${actions.length} actions`);
} else {
  mkdirSync(dirname(out), { recursive: true });
  writeFileSync(out, text);
  console.log(`wrote ${out}: ${components.length} components, ${actions.length} actions`);
}
