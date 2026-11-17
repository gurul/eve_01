// After `pnpm build` and `pnpm build:web`: prints BUNDLES_OK only when the
// app's bundle (../App/ChatWeb/chat.js) and eve lite's (web_lite/assets/*.js)
// both carry the json-render card (its fence, its marker attribute and the
// catalog's action names), beside the OpenUI fence as a control that the
// search reads the right file.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const lite = resolve(here, "../../../../bridge/src/eve_bridge/web_lite/assets");
const files = [
  resolve(here, "../../App/ChatWeb/chat.js"),
  ...readdirSync(lite).filter((f) => f.endsWith(".js")).map((f) => resolve(lite, f)),
];
const needles = ["```json-render", "data-engine", "open_url", "```openui-lang"];
let bad = files.length < 2 ? 1 : 0;
for (const file of files) {
  const text = readFileSync(file, "utf8");
  const missing = needles.filter((n) => !text.includes(n));
  const age = Math.round((Date.now() - statSync(file).mtimeMs) / 1000);
  console.log(`${missing.length ? "FAIL" : "pass"}  ${file}: ${(text.length / 1e6).toFixed(2)} MB, built ${age}s ago${missing.length ? `, missing ${missing.join(", ")}` : ""}`);
  if (missing.length || age > 600) bad += 1;
}
if (bad) { console.error("NOT OK"); process.exit(1); }
console.log("BUNDLES_OK");
