#!/usr/bin/env node
// Runs check.mjs over every model in Eve/, asking about every theorem each
// file declares: none may use sorry, native_decide or a non-standard axiom.
import { readdirSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
let failed = 0, total = 0;
for (const f of readdirSync(join(here, "Eve")).filter((f) => f.endsWith(".lean")).sort()) {
  const src = readFileSync(join(here, "Eve", f), "utf8");
  const ns = (src.match(/^namespace\s+(\S+)/m) || [])[1];
  const names = [...src.matchAll(/^(?:private\s+)?theorem\s+([A-Za-z_0-9']+)/gm)].map((m) => (ns ? `${ns}.${m[1]}` : m[1]));
  // Two honest shapes: a model that found a bug proves the old code breaks the property (current_violates)
  // and the fixed code keeps it (fixed_invariant); a model of code that was right from the start proves the
  // property of the code as it is (code_invariant). Nothing else passes.
  const bug = names.includes(`${ns}.current_violates`) && names.includes(`${ns}.fixed_invariant`);
  const sound = names.includes(`${ns}.code_invariant`);
  if (!bug && !sound) {
    console.error(`FAIL Eve/${f}: needs current_violates + fixed_invariant, or code_invariant`);
    failed++;
    continue;
  }
  const r = spawnSync(process.execPath, [join(here, "check.mjs"), `Eve/${f}`, ...names], { encoding: "utf8" });
  const out = (r.stdout || "") + (r.stderr || "");
  if (r.status !== 0) { console.error(out); failed++; }
  else { console.log(out.trim().split("\n").pop()); total += names.length; }
}
if (failed) { console.error(`FAIL ${failed} model(s)`); process.exit(1); }
console.log(`ALL_MODELS_OK ${total} theorems`);
