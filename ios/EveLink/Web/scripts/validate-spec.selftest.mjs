// The validator must fail what it should fail. Runs scripts/validate-spec.mjs
// as a process, on one good block and three broken ones, and prints
// VALIDATOR_SELFTEST_OK only when the good one exits 0 and each broken one
// exits 1 with a reason.
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const script = resolve(dirname(fileURLToPath(import.meta.url)), "validate-spec.mjs");
const line = (path, value) => JSON.stringify({ op: "add", path, value });
const good = [
  line("/root", "card"),
  line("/elements/card", { type: "Card", props: { title: "Runs" }, children: ["table"] }),
  line("/elements/table", { type: "Table", props: { columns: ["Day", "km"], rows: [["Mon", 5]] }, children: [] }),
].join("\n");
const cases = [
  ["good", good, 0],
  ["unknown component", good.replace('"Table"', '"Grid"'), 1],
  ["dangling child", good.replace('["table"]', '["table","gone"]'), 1],
  ["cut-off stream", good.slice(0, -25), 1],
];
let failed = 0;
for (const [name, block, want] of cases) {
  const run = spawnSync(process.execPath, [script], { input: block, encoding: "utf8" });
  const said = `${run.stdout}${run.stderr}`.trim().split("\n")[0];
  const ok = run.status === want && (want === 0 ? /^ok: root Card$/.test(said) : said.length > 0);
  console.log(`${ok ? "pass" : "FAIL"}  ${name}: exit ${run.status} (want ${want}) — ${said}`);
  if (!ok) failed += 1;
}
const batch = spawnSync(process.execPath, [script, "--batch"], { input: JSON.stringify(cases.map((c) => c[1])), encoding: "utf8" });
const results = JSON.parse(batch.stdout);
const batchOk = batch.status === 1 && results.map((r) => r.ok).join() === "true,false,false,false";
console.log(`${batchOk ? "pass" : "FAIL"}  --batch: exit ${batch.status}, ok = ${results.map((r) => r.ok).join()}`);
if (failed || !batchOk) process.exit(1);
console.log("VALIDATOR_SELFTEST_OK");
