import { describe, expect, it } from "vitest";
import { catalog } from "../jsonCatalog";
import { compileSpec, inspectSpec, selectSpec } from "../specProgram";

const line = (path: string, value: unknown) => JSON.stringify({ op: "add", path, value });
const el = (key: string, type: string, props: unknown, children: string[] = [], more: object = {}) =>
  line(`/elements/${key}`, { type, props, children, ...more });
const GOOD = [
  line("/root", "card"),
  el("card", "Card", { title: "Dinner", subtitle: "For two" }, ["text", "table"]),
  el("text", "Text", { text: "Roast vegetables" }),
  el("table", "Table", { columns: ["Dish", "Minutes"], rows: [["Soup", 20], ["Tart", 45]] }),
].join("\n");
const FORM = [
  line("/root", "card"),
  el("card", "Card", {}, ["city", "go"]),
  el("city", "TextInput", { label: "City", value: { $bindState: "/form/city" } }),
  line("/state/form", { city: "" }),
  el("go", "Button", { label: "Search" }, [], { on: { press: { action: "ask", params: { message: "Find hotels", data: { $state: "/form" } } } } }),
].join("\n");

describe("json-render blocks", () => {
  it("builds a spec from patch lines and recovers readable text", () => {
    const result = inspectSpec(GOOD);
    expect(result.complete).toBe(true);
    expect(result.issues).toEqual([]);
    expect(result.spec?.root).toBe("card");
    expect(result.text).toBe("Dinner\n\nFor two\n\nRoast vegetables\n\nDish: Soup · Minutes: 20\n\nDish: Tart · Minutes: 45");
    expect(result.hasControls).toBe(false);
  });

  it("takes one whole JSON spec as well as patch lines", () => {
    const whole = JSON.stringify(compileSpec(GOOD).spec, null, 2);
    expect(inspectSpec(whole).complete).toBe(true);
    expect(inspectSpec(whole).text).toBe(inspectSpec(GOOD).text);
  });

  it("accepts dynamic values where a component takes a plain one, and knows a form has controls", () => {
    const result = inspectSpec(FORM);
    expect(result.issues).toEqual([]);
    expect(result.complete).toBe(true);
    expect(result.hasControls).toBe(true);
    // the readable text never carries bindings, actions or state
    expect(result.text).toBe("City\n\nSearch");
  });

  it("fills in what a leaf may leave out, and nothing else", () => {
    const lean = [line("/root", "c"), line("/elements/c", { type: "Card", props: {}, children: ["t"] }),
      line("/elements/t", { type: "Text", props: { text: "Hi" } })].join("\n");
    expect(inspectSpec(lean).complete).toBe(true);
  });

  it.each([
    ["a component outside the catalog", GOOD.replace('"Table"', '"Grid"')],
    ["a child that was never added", GOOD.replace('["text","table"]', '["text","table","gone"]')],
    ["a cut-off stream", GOOD.slice(0, -30)],
    ["props the component does not take", GOOD.replace('[["Soup",20],["Tart",45]]', '"Soup, 20"')],
    ["a root that is not a card", GOOD.replace('"type":"Card"', '"type":"Stack"')],
    ["a card with nothing in it", [line("/root", "card"), el("card", "Card", { title: "Empty" })].join("\n")],
    ["prose where a patch belongs", `${GOOD}\nThat is the plan.`],
    ["no block at all", ""],
  ])("does not call %s complete", (_name, code) => {
    const result = inspectSpec(code);
    expect(result.complete).toBe(false);
    expect(result.issues.length).toBeGreaterThan(0);
    expect(selectSpec(result, undefined, false)).toBeUndefined();
  });

  it("draws what has arrived while streaming, then holds the last card through a bad update", () => {
    const cut = GOOD.slice(0, GOOD.lastIndexOf("\n") + 40);         // the last line is half written
    const partial = inspectSpec(cut, true);
    expect(partial.renderable).toBe(true);
    expect(partial.complete).toBe(false);
    // the half-written child is left out of what the renderer gets, so nothing dangles
    expect(partial.spec?.elements["card"]?.children).toEqual(["text"]);
    expect(selectSpec(partial, undefined, true)).toBe(partial);
    const nothing = inspectSpec("{", true);
    expect(nothing.renderable).toBe(false);
    expect(selectSpec(nothing, partial, true)).toBe(partial);
    expect(selectSpec(nothing, partial, false)).toBeUndefined();
    expect(selectSpec(inspectSpec(GOOD), partial, false)?.complete).toBe(true);
  });

  it("shows an element nothing holds at the end of a finished card, and leaves it out while streaming", () => {
    const code = `${GOOD}\n${el("extra", "Text", { text: "One more thing" })}`;
    const done = inspectSpec(code);
    expect(done.complete).toBe(true);
    expect(done.adopted).toEqual(["extra"]);
    expect(done.spec?.elements["card"]?.children).toEqual(["text", "table", "extra"]);
    expect(done.text.endsWith("One more thing")).toBe(true);
    const live = inspectSpec(code, true);
    expect(live.adopted).toEqual([]);
    expect(live.spec?.elements["card"]?.children).toEqual(["text", "table"]);
    expect(inspectSpec(GOOD).adopted).toEqual([]);                // control: nothing to adopt in a whole card
  });

  it("checks every component the catalog names", () => {
    expect(catalog.componentNames).toContain("Card");
    expect(catalog.actionNames).toEqual(expect.arrayContaining(["ask", "open_url"]));
  });
});
