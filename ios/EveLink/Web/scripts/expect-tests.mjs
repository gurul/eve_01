// Reads vitest's verbose output on stdin. Prints WEB_TESTS_OK only when the
// run passed with no failed test and every test file named in the arguments
// ran at least one passing test. Usage:
//   pnpm test -- --reporter=verbose 2>&1 | node scripts/expect-tests.mjs specProgram JsonCard
let text = "";
for await (const chunk of process.stdin) text += chunk;
const plain = text.replace(/\x1b\[[0-9;]*m/g, "");
const problems = [];
if (/\b\d+ failed\b/.test(plain) || /\bFAIL\b/.test(plain)) problems.push("a test failed");
const total = /Tests\s+(\d+) passed/.exec(plain);
if (!total) problems.push("no 'Tests N passed' line");
for (const name of process.argv.slice(2)) {
  const ran = plain.split("\n").filter((l) => l.includes(`${name}.test.ts`) && /[✓√]/.test(l)).length;
  if (!ran) problems.push(`no passing test from ${name}.test.ts`);
  else console.log(`${name}.test.ts: ${ran} passing`);
}
if (problems.length) {
  console.error(plain.split("\n").slice(-40).join("\n"));
  console.error(`NOT OK: ${problems.join("; ")}`);
  process.exit(1);
}
console.log(`WEB_TESTS_OK (${total[1]} tests passed)`);
