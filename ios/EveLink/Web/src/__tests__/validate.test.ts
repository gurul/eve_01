import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// scripts/validate.mjs as bridge/tools/openui_eval.py runs it: a JSON batch piped to stdin, results read from stdout.
const SCRIPT = fileURLToPath(new URL("../../scripts/validate.mjs", import.meta.url));
const GOOD = 'root = Card([header])\nheader = CardHeader("Descale the kettle", "About 30 minutes")\n';

function run(input: string, ...args: string[]) {
  return spawnSync(process.execPath, [SCRIPT, ...args], { input, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
}

describe("validate.mjs", () => {
  it("checks a batch larger than 1 MB through a pipe and returns every result", () => {
    const programs = Array.from({ length: 12000 }, (_, i) => (i === 7 ? "root = Card([missing])" : GOOD));
    const input = JSON.stringify(programs);
    expect(input.length).toBeGreaterThan(1024 * 1024);
    const p = run(input, "--batch");
    expect(p.stderr).toBe("");
    const results = JSON.parse(p.stdout) as { ok: boolean; errors: string[] }[];
    expect(results).toHaveLength(programs.length);
    expect(results.filter((r) => !r.ok)).toHaveLength(1);
    expect(results[7]?.errors).toEqual(["unresolved: missing is used but never defined"]);
    expect(p.status).toBe(1);
  }, 60_000);

  it("passes one good program from stdin", () => {
    const p = run(GOOD);
    expect(p.status).toBe(0);
    expect(p.stdout).toContain("ok: root Card");
  });
});
