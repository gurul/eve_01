// Prints DOCS_OK only when the three documents that describe the chat say how
// the json-render trial is turned on, and the tools they name exist.
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");
const read = (p) => readFileSync(resolve(root, p), "utf8");
const pkg = JSON.parse(read("ios/EveLink/Web/package.json"));
const checks = [
  ["README.md names the switch and links the section", () => /EVE_CARD_ENGINE=json-render/.test(read("README.md")) && read("README.md").includes("docs/app-chat.md#the-card-engine")],
  ["docs/app-chat.md has the section the README links", () => read("docs/app-chat.md").includes("\n## The card engine\n")],
  ["docs/app-chat.md says how to turn it on and how to go back", () => { const d = read("docs/app-chat.md"); return d.includes("EVE_CARD_ENGINE=json-render") && d.includes("**To go back:**") && d.includes("build.sh --install"); }],
  ["the web README names the scripts", () => ["gen-json-prompt", "check-json-prompt", "validate-spec", "preview-cards"].every((s) => read("ios/EveLink/Web/README.md").includes(`pnpm ${s}`))],
  ["every script the docs name is in package.json", () => ["gen-json-prompt", "check-json-prompt", "validate-spec", "preview-cards"].every((s) => typeof pkg.scripts[s] === "string")],
  ["every file the docs name exists", () => ["ios/EveLink/Web/src/jsonCatalog.ts", "ios/EveLink/Web/src/jsonRegistry.tsx", "ios/EveLink/Web/src/specProgram.ts", "ios/EveLink/Web/src/JsonCard.tsx", "ios/EveLink/Web/preview.html", "bridge/src/eve_bridge/json_render_prompt.txt", "bridge/tools/card_engine_check.py", "bridge/tools/card_eval_gate.py"].every((f) => existsSync(resolve(root, f)))],
];
let bad = 0;
for (const [name, ok] of checks) {
  const pass = ok();
  console.log(`${pass ? "pass" : "FAIL"}  ${name}`);
  if (!pass) bad += 1;
}
if (bad) { console.error("NOT OK"); process.exit(1); }
console.log("DOCS_OK");
