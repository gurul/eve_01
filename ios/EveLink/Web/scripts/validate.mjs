// Checks OpenUI Lang programs with the parser and component library the app
// renders with (@openuidev/react-lang's createParser over openuiChatLibrary,
// exactly as the Renderer in src/AssistantMessage.tsx builds it).
//
//   node scripts/validate.mjs program.txt        one program from a file
//   node scripts/validate.mjs < program.txt      one program from stdin
//   node scripts/validate.mjs --batch < in.json  a JSON array of programs; prints a JSON array of results
//
// A program passes when it parses to a root of the library's root component
// (Card), with no validation error (an unknown component, a missing required
// argument, too many arguments, a wrong type), no reference left undefined,
// and no truncated input. Statements nothing reaches from root are reported
// as `orphaned` but do not fail it: the app renders the rest. Parser warnings
// (a Card entry not named `root`) are reported as `warnings` and do not fail it
// either: the app renders those too.
//
// Exit status: 0 when every program passes, 1 when one fails, 2 on bad usage.
// Input is read as a stream and output is left to drain, so a batch of any size
// works through a pipe.
// The packages are resolved from this file's folder, so the script works when
// called from anywhere (bridge/tools/openui_eval.py runs it from a checkout
// that may not have its own node_modules).

process.env.OPENUI_TELEMETRY_DISABLED = "1";
process.env.DO_NOT_TRACK = "1";

import { readFileSync, realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

const { openuiChatLibrary } = await import("@openuidev/react-ui/genui-lib");
const { createParser } = await import("@openuidev/react-lang");

const parser = createParser(openuiChatLibrary.toJSONSchema(), openuiChatLibrary.root);

/** One program → { ok, root, errors, warnings, orphaned }. Never throws. */
export function check(code) {
  const text = String(code ?? "");
  let result;
  try {
    result = parser.parse(text);
  } catch (e) {
    return { ok: false, root: null, errors: [`parse-exception: ${e instanceof Error ? e.message : String(e)}`], warnings: [], orphaned: [] };
  }
  const errors = [];
  const warnings = [];
  const meta = result.meta;
  if (!result.root) errors.push("no root: the program must define root = Card(...)");
  else if (openuiChatLibrary.root && result.root.typeName !== openuiChatLibrary.root)
    errors.push(`root is ${result.root.typeName}, not ${openuiChatLibrary.root}`);
  if (meta.incomplete) errors.push("incomplete: the program is cut off");
  for (const name of meta.unresolved) errors.push(`unresolved: ${name} is used but never defined`);
  // the parser's unknown-component message lists the whole catalog after " Available components:"; one line is enough
  for (const e of meta.errors) (e.severity === "warning" ? warnings : errors).push(`${e.code}: ${e.message.split(" Available components:")[0]}`);
  return { ok: errors.length === 0, root: result.root?.typeName ?? null, errors, warnings, orphaned: meta.orphaned };
}

/** All of stdin, read as a stream: readFileSync(0) fails with EAGAIN on a large pipe (over the 64 KB pipe buffer). */
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
    console.error("usage: validate.mjs [program.txt] | validate.mjs --batch < programs.json");
    process.exit(2);
  }
  const input = files[0] ? readFileSync(files[0], "utf8") : await readStdin();
  if (batch) {
    let programs;
    try {
      programs = JSON.parse(input);
    } catch {
      console.error("--batch reads a JSON array of strings");
      process.exit(2);
    }
    if (!Array.isArray(programs)) {
      console.error("--batch reads a JSON array of strings");
      process.exit(2);
    }
    const results = programs.map(check);
    // exitCode, not exit(): exit() drops whatever of a large result a pipe has not taken yet
    process.stdout.write(`${JSON.stringify(results)}\n`);
    process.exitCode = results.every((r) => r.ok) ? 0 : 1;
  } else {
    const r = check(input);
    if (r.ok) console.log(`ok: root ${r.root}${r.orphaned.length ? ` (orphaned: ${r.orphaned.join(", ")})` : ""}`);
    else for (const e of r.errors) console.error(e);
    process.exitCode = r.ok ? 0 : 1;
  }
}
