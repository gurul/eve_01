import { describe, expect, it } from "vitest";
import { inspectUi, selectUi, UiStateCache } from "../uiProgram";

const GOOD = 'root = Card([h, t])\nh = CardHeader("Dinner", "For two")\nt = TextContent("Roast vegetables")';

describe("generated card recovery", () => {
  it("validates real catalog nodes and recovers readable text", () => {
    const result = inspectUi(GOOD);
    expect(result.complete).toBe(true);
    expect(result.text).toBe("Dinner\n\nFor two\n\nRoast vegetables");
    expect(result.hasControls).toBe(false);
  });

  it("renders meaningful partial input, then holds the last card through a malformed update", () => {
    const partial = inspectUi(GOOD.slice(0, -3));
    expect(partial.renderable).toBe(true);
    expect(partial.complete).toBe(false);
    expect(selectUi(partial, undefined, true)).toBe(partial);
    const broken = inspectUi('root = Card([t])\nt = TextContent(99)');
    expect(broken.renderable).toBe(false); // positive control for validation
    expect(selectUi(broken, partial, true)).toBe(partial);
    // Final failure must not masquerade as a successfully finished partial card.
    expect(selectUi(broken, partial, false)).toBeUndefined();
    expect(selectUi(inspectUi(GOOD), partial, false)?.complete).toBe(true);
  });

  it("rejects unresolved, empty, unknown and wrong-root final programs", () => {
    for (const code of ['root = Card([missing])', 'root = Card([])', 'root = MissingCard([])', 'root = TextContent("Hi")', 'not a program']) {
      expect(inspectUi(code).complete, code).toBe(false);
      expect(selectUi(inspectUi(code), undefined, false), code).toBeUndefined();
    }
  });

  it("keeps partial failures explicit even when the parser salvages some content", () => {
    const result = inspectUi(`${GOOD.replace("[h, t]", "[h, t, bad]")}\nbad = TextContent(99)`);
    expect(result.complete).toBe(false);
    expect(result.text).toContain("Roast vegetables");
  });

  it("does not mistake an unresolved forward reference for a finished card", () => {
    const result = inspectUi('root = Card([h, missing])\nh = CardHeader("Dinner")');
    expect(result.renderable).toBe(true);
    expect(result.complete).toBe(false);
    expect(selectUi(result, undefined, true)).toBe(result);
    expect(selectUi(result, undefined, false)).toBeUndefined();
  });
});

describe("readable generated content", () => {
  it("preserves table row and column associations, including zero", () => {
    const result = inspectUi('root = Card([t])\nt = Table([Col("Option", ["Walk", "Train"]), Col("Cost", [0, 4])])');
    expect(result.complete).toBe(true);
    expect(result.text).toBe("Option: Walk · Cost: 0\n\nOption: Train · Cost: 4");
  });

  it("preserves boolean table comparisons and marks missing cells", () => {
    const result = inspectUi('root = Card([Table([Col("Available", [true, false]), Col("Count", [0])])])');
    expect(result.complete).toBe(true);
    expect(result.text).toBe("Available: true · Count: 0\n\nAvailable: false · Count: —");
  });

  it("keeps chart labels, units, series names and numeric values together", () => {
    const result = inspectUi('root = Card([c])\nc = BarChart(["Mon", "Tue"], [Series("Minutes", [0, 25])], "grouped", "Day", "Time")');
    expect(result.complete).toBe(true);
    expect(result.text).toBe("Day\n\nTime\n\nMon · Minutes · 0\n\nTue · Minutes · 25");
  });

  it("extracts both tabs and steps rather than only the initially visible panel", () => {
    const result = inspectUi('root = Card([Tabs([TabItem("a", "Today", [Steps([StepsItem("Prep", "Chop vegetables")])]), TabItem("b", "Tomorrow", [TextContent("Use leftovers")])])])');
    expect(result.complete).toBe(true);
    expect(result.text).toBe("Today\n\nPrep\n\nChop vegetables\n\nTomorrow\n\nUse leftovers");
  });

  it("excludes action payloads, form IDs, hidden password contents and runtime ASTs", () => {
    const result = inspectUi('root = Card([f])\nf = Form("private-form", Buttons([Button("Continue", @ToAssistant("private-action"))]), [FormControl("Name", Input("private-field", "Your name")), FormControl("Password", Input("secret-field", "secret-placeholder", "password"))])');
    expect(result.complete).toBe(true);
    expect(result.hasControls).toBe(true);
    expect(result.text).toBe("Name\n\nYour name\n\nPassword\n\nContinue");
    expect(result.text).not.toMatch(/private|secret|ToAssistant/);
  });

  it("does not scrape orphaned string literals into the answer", () => {
    const result = inspectUi(`${GOOD}\nunused = "not part of the answer"`);
    expect(result.complete).toBe(true);
    expect(result.text).not.toContain("not part");
  });
});

describe("page-local form state", () => {
  it("isolates cards, bounds retained states and resets only the selected card", () => {
    const cache = new UiStateCache(2);
    cache.set("first:0", { form: { name: { value: "A" } } });
    cache.set("first:1", { form: { name: { value: "B" } } });
    expect(cache.get("first:0")).not.toEqual(cache.get("first:1"));
    cache.set("second:0", { form: { name: { value: "C" } } });
    expect(cache.get("first:0")).toBeUndefined();
    expect(cache.get("first:1")).toBeDefined();
    cache.delete("first:1");
    expect(cache.get("first:1")).toBeUndefined();
    expect(cache.get("second:0")).toEqual({ form: { name: { value: "C" } } });
  });
});
