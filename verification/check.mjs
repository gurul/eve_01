#!/usr/bin/env node
// Checks one Lean model file the way a reviewer would, so a proof cannot pass
// by leaving a hole in it.
//
//   node verification/check.mjs Eve/Command.lean Eve.Command.allow_is_simple ...
//
// It compiles the file with the pinned toolchain, fails on any error or any
// `sorry`, then asks Lean which axioms each named theorem rests on and fails
// unless that set is inside Lean's three standard ones. A theorem closed by
// `native_decide` (trusts compiled code) or `sorryAx` is refused.
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir, homedir } from "node:os";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const [file, ...names] = process.argv.slice(2);
if (!file || names.length === 0) {
  console.error("usage: check.mjs <Eve/File.lean> <theorem> [<theorem> ...]");
  process.exit(2);
}
const ALLOWED = new Set(["propext", "Classical.choice", "Quot.sound"]);
const lean = process.env.LEAN || join(homedir(), ".elan", "bin", "lean");

const src = readFileSync(resolve(here, file), "utf8");
if (/\bsorry\b|\badmit\b/.test(src.replace(/--.*$/gm, "").replace(/\/-[\s\S]*?-\//g, ""))) {
  console.error(`FAIL ${file}: contains sorry/admit`);
  process.exit(1);
}
const probe = src + "\n" + names.map((n) => `#print axioms ${n}`).join("\n") + "\n";
const dir = mkdtempSync(join(tmpdir(), "eve-lean-"));
const tmp = join(dir, "Probe.lean");
writeFileSync(tmp, probe);
const r = spawnSync(lean, [tmp], { cwd: here, encoding: "utf8", timeout: 600_000 });
const out = (r.stdout || "") + (r.stderr || "");
if (r.status !== 0 || /error:/.test(out)) {
  console.error(out);
  console.error(`FAIL ${file}: lean exited ${r.status}`);
  process.exit(1);
}
if (/declaration uses 'sorry'/.test(out)) {
  console.error(out);
  console.error(`FAIL ${file}: a declaration uses sorry`);
  process.exit(1);
}
let bad = 0;
for (const n of names) {
  const none = new RegExp(`'${n.replace(/\./g, "\\.")}' does not depend on any axioms`);
  const some = new RegExp(`'${n.replace(/\./g, "\\.")}' depends on axioms: \\[([^\\]]*)\\]`);
  if (none.test(out)) { console.log(`ok   ${n}: no axioms`); continue; }
  const m = out.match(some);
  if (!m) { console.error(`FAIL ${n}: not found (unknown constant?)`); bad++; continue; }
  const axioms = m[1].split(",").map((s) => s.trim()).filter(Boolean);
  const extra = axioms.filter((a) => !ALLOWED.has(a));
  if (extra.length) { console.error(`FAIL ${n}: rests on ${extra.join(", ")}`); bad++; }
  else console.log(`ok   ${n}: ${axioms.join(", ")}`);
}
if (bad) process.exit(1);
console.log(`LEAN_CHECK_OK ${file} (${names.length} theorems)`);
