// Checks json-render blocks with the code the app draws them with
// (src/specProgram.ts over the catalog in src/jsonCatalog.ts).
//
//   node scripts/validate-spec.mjs block.txt        one block from a file
//   node scripts/validate-spec.mjs < block.txt      one block from stdin
//   node scripts/validate-spec.mjs --batch < in.json  a JSON array of blocks; prints a JSON array of results
//
// A block is what goes inside the ```json-render fence: one JSON Patch per
// line. It passes when every line is a patch, the root is a Card with a
// child, every element is in the catalog with props its component takes, and
// every child it names is there: what the app requires of a finished card.
// An element that no children list names is reported as `adopted` and does not
// fail the block: the app shows it at the end of the card.
//
// Exit status: 0 when every block passes, 1 when one fails, 2 on bad usage.
// Node 22.18 or later (it runs the TypeScript sources as they are).
import { readFileSync, realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { inspectSpec } from "../src/specProgram.ts";

/** One block → { ok, root, errors }. Never throws. */
export function check(code) {
  try {
    const result = inspectSpec(String(code ?? ""));
    const root = result.spec ? String(result.spec.elements[result.spec.root]?.type ?? "") : null;
    return {
      ok: result.complete,
      root,
      errors: result.complete ? [] : (result.issues.length ? result.issues : ["not a complete card"]),
      // elements no children list named: the app shows them at the end of the card
      adopted: result.adopted,
    };
  } catch (e) {
    return { ok: false, root: null, errors: [`exception: ${e instanceof Error ? e.message : String(e)}`], adopted: [] };
  }
}

async function readStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(typeof chunk === "string" ? Buffer.from(chunk) : chunk);
  return Buffer.concat(chunks).toString("utf8");
}

const isMain = Boolean(process.argv[1]) && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url));
if (isMain) {
  const args = process.argv.slice(2);
  const batch = args.includes("--batch");
  const files = args.filter((a) => !a.startsWith("--"));
  if (files.length > 1 || (batch && files.length)) {
    console.error("usage: validate-spec.mjs [block.txt] | validate-spec.mjs --batch < blocks.json");
    process.exit(2);
  }
  const input = files[0] ? readFileSync(files[0], "utf8") : await readStdin();
  if (batch) {
    let blocks;
    try { blocks = JSON.parse(input); } catch { blocks = null; }
    if (!Array.isArray(blocks)) {
      console.error("--batch reads a JSON array of strings");
      process.exit(2);
    }
    const results = blocks.map(check);
    process.stdout.write(`${JSON.stringify(results)}\n`);
    process.exitCode = results.every((r) => r.ok) ? 0 : 1;
  } else {
    const r = check(input);
    if (r.ok) console.log(`ok: root ${r.root}${r.adopted.length ? ` (no parent, shown last: ${r.adopted.join(", ")})` : ""}`);
    else for (const e of r.errors) console.error(e);
    process.exitCode = r.ok ? 0 : 1;
  }
}
