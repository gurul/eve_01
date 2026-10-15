// The docs say what the code does: no doc says a tap on an Update sends it, docs/muse.md covers the Pi and the switch,
// and the README links it. A positive control proves the "tap sends" pattern still matches the old wording.
import { existsSync, readFileSync } from "node:fs";

const tapSends = /\b(a )?tap (on a card )?sends\b/i;
if (!tapSends.test("A tap on a card sends that sentence to eve in Chat.")) {
  console.log("FAIL: positive control not caught"); process.exit(1);
}
const problems = [];
for (const file of ["README.md", "docs/updates.md"]) {
  readFileSync(file, "utf8").split("\n").forEach((line, i) => {
    if (/update|card/i.test(line) && tapSends.test(line)) problems.push(`${file}:${i + 1} still says a tap sends`);
  });
}
if (!existsSync("docs/muse.md")) problems.push("docs/muse.md is missing");
else {
  const muse = readFileSync("docs/muse.md", "utf8");
  for (const need of ["EVE_MUSE_PI", "left of the", "History", "Raspberry Pi", "muse off", "pi/muse/"])
    if (!muse.includes(need)) problems.push(`docs/muse.md lacks "${need}"`);
}
if (!readFileSync("README.md", "utf8").includes("(docs/muse.md)")) problems.push("README.md does not link docs/muse.md");
if (problems.length) { console.log(problems.join("\n")); process.exit(1); }
console.log("DOCS_OK");
