// A json-render block, from the text of a reply to a spec the page can draw.
// The block is SpecStream (one RFC 6902 patch per line), or one whole JSON
// spec. Nothing in it is executed: it is data, checked against the catalog
// (jsonCatalog.ts) before the final card is shown. No React here, so
// scripts/validate-spec.mjs runs the same code in Node.
import { applySpecStreamPatch, autoFixSpec, parseSpecStreamLine, validateSpec, type Spec } from "@json-render/core";
import { catalog, CONTROLS, ROOT } from "./jsonCatalog.ts";

type Element = { type?: unknown; props?: unknown; children?: unknown; [k: string]: unknown };
type Loose = { root?: unknown; elements?: Record<string, Element>; state?: unknown };

const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const scalar = (v: unknown): string =>
  typeof v === "string" || typeof v === "number" || typeof v === "boolean" ? String(v) : "";
const array = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
const known = new Set(catalog.componentNames);

/** The block's lines applied in order. `bad` counts lines that are not a patch, or that could not be applied. */
export function compileSpec(code: string, streaming = false): { spec: Loose; bad: number } {
  const whole = code.trim();
  if (whole.startsWith("{")) {
    try {
      const parsed: unknown = JSON.parse(whole);
      if (record(parsed) && record(parsed["elements"]) && !("op" in parsed)) return { spec: parsed as Loose, bad: 0 };
    } catch { /* not one object: read it line by line */ }
  }
  const lines = code.split("\n").map((l) => l.trim()).filter(Boolean);
  const spec: Record<string, unknown> = {};
  let bad = 0;
  lines.forEach((line, i) => {
    const patch = parseSpecStreamLine(line);
    // the last line of a streaming block is still being written
    if (!patch) { if (!(streaming && i === lines.length - 1)) bad += 1; return; }
    try { applySpecStreamPatch(spec, patch); } catch { bad += 1; }
  });
  return { spec: spec as Loose, bad };
}

/**
 * The spec as the renderer takes it: every element has props and children,
 * and a child that is not there (yet), or is not in the catalog, is left out.
 * `pruned` says whether anything was left out.
 */
function normalize(loose: Loose, adopt: boolean): { spec: Spec | null; pruned: boolean; adopted: string[] } {
  const elements = record(loose.elements) ? loose.elements : {};
  const root = typeof loose.root === "string" ? loose.root : "";
  let pruned = false;
  const out: Record<string, Element> = {};
  for (const [key, el] of Object.entries(elements)) {
    if (!record(el) || typeof el.type !== "string" || !known.has(el.type)) { pruned = true; continue; }
    out[key] = { ...el, props: record(el.props) ? el.props : {}, children: array(el.children) };
  }
  for (const el of Object.values(out)) {
    const kept = array(el.children).filter((c) => typeof c === "string" && c in out);
    if (kept.length !== array(el.children).length) pruned = true;
    el.children = kept;
  }
  if (!root || !(root in out)) return { spec: null, pruned, adopted: [] };
  // An element no children list names would never be drawn: the model wrote
  // content and forgot to hang it on the card. In a finished block it goes at
  // the end of the root card, in the order it was written, rather than being
  // dropped without a word. (Not while streaming: a child may arrive before
  // the parent that names it.)
  const adopted: string[] = [];
  if (adopt) {
    const held = new Set(Object.values(out).flatMap((el) => array(el.children).map(String)));
    for (const key of Object.keys(out)) if (key !== root && !held.has(key)) adopted.push(key);
    if (adopted.length) out[root]!.children = [...array(out[root]!.children), ...adopted];
  }
  const spec = { root, elements: out, ...(record(loose.state) ? { state: loose.state } : {}) } as unknown as Spec;
  return { spec: autoFixSpec(spec, { lossy: false }).spec, pruned, adopted };
}

/** Static, selectable content; never serializes actions, bindings or state. */
export function readableSpec(spec: Spec | null): string {
  if (!spec) return "";
  const seen = new Set<string>();
  function read(key: string, depth: number): string[] {
    const el = spec!.elements[key] as Element | undefined;
    if (!el || seen.has(key) || depth > 40) return [];
    seen.add(key);
    const p = record(el.props) ? el.props : {};
    const text = (...keys: string[]) => keys.map((k) => scalar(p[k])).filter(Boolean);
    const kids = () => array(el.children).flatMap((c) => read(String(c), depth + 1));
    const rows = (k: string) => array(p[k]).filter(record);
    switch (el.type) {
      case "Card": return [...text("title", "subtitle"), ...kids()];
      case "Text": return text("text");
      case "Callout": return text("title", "description");
      case "Table": {
        const cols = array(p["columns"]).map(scalar);
        return array(p["rows"]).map((row) =>
          array(row).map((c, i) => `${cols[i] ?? ""}: ${scalar(c) || "—"}`).join(" · "));
      }
      case "Facts": return rows("rows").map((r) => [scalar(r["label"]), scalar(r["value"])].filter(Boolean).join(": "));
      case "List": return rows("items").map((r) => [scalar(r["title"]), scalar(r["subtitle"])].filter(Boolean).join(": "));
      case "Steps": return rows("items").map((r, i) =>
        `${i + 1}. ${[scalar(r["title"]), scalar(r["details"])].filter(Boolean).join(": ")}`);
      case "BarChart":
      case "LineChart": {
        const labels = array(p["labels"]);
        return [...text("xLabel", "yLabel"), ...rows("series").flatMap((s) =>
          array(s["values"]).map((v, i) => [scalar(labels[i]), scalar(s["name"]), scalar(v)].filter(Boolean).join(" · ")))];
      }
      case "PieChart": return array(p["values"]).map((v, i) => `${scalar(array(p["labels"])[i])}: ${scalar(v)}`);
      case "Image": return text("alt");
      case "Tags": return [array(p["tags"]).map(scalar).filter(Boolean).join(" · ")];
      case "Button": return text("label");
      case "FollowUps": return array(p["items"]).map(scalar);
      case "Tabs":
      case "Accordion": {
        const names = array(p[el.type === "Tabs" ? "tabs" : "sections"]).map(scalar);
        return array(el.children).flatMap((c, i) => [names[i] ?? "", ...read(String(c), depth + 1)]);
      }
      case "TextInput":
      case "Select":
      case "Chips": return text("label");
      default: return kids();
    }
  }
  return read(spec.root, 0).filter(Boolean).join("\n\n");
}

const dynamic = (v: unknown): boolean => record(v) && Object.keys(v).some((k) => k.startsWith("$"));

/**
 * Each element's props against its component's schema. A value that is a
 * dynamic expression ({ "$state": … }, { "$bindState": … }) is resolved at
 * render time, so a mismatch at or under one is not an issue.
 */
function propIssues(spec: Spec): string[] {
  const components = (catalog.data as { components: Record<string, { props?: { safeParse(v: unknown): { success: boolean; error?: { issues: { path: PropertyKey[]; message: string }[] } } } }> }).components;
  const out: string[] = [];
  for (const [key, el] of Object.entries(spec.elements)) {
    const schema = components[String(el.type)]?.props;
    if (!schema) continue;
    const result = schema.safeParse(el.props);
    if (result.success) continue;
    for (const issue of result.error?.issues ?? []) {
      let at: unknown = el.props;
      let skip = dynamic(at);
      for (const step of issue.path) {
        at = record(at) || Array.isArray(at) ? (at as Record<PropertyKey, unknown>)[step] : undefined;
        if (dynamic(at)) { skip = true; break; }
      }
      if (!skip) out.push(`${key} (${String(el.type)}) ${issue.path.map(String).join("/")}: ${issue.message}`);
    }
  }
  return out.slice(0, 8);
}

export interface SpecProgram {
  code: string;
  /** What the renderer is given: null when there is no root card yet. */
  spec: Spec | null;
  text: string;
  renderable: boolean;
  complete: boolean;
  hasControls: boolean;
  /** Why a finished block is not complete, for the validator's output; never shown or logged by the page. */
  issues: string[];
  /** Elements no children list named, which a finished card shows at its end (see normalize). */
  adopted: string[];
}

export function inspectSpec(code: string, streaming = false): SpecProgram {
  const { spec: loose, bad } = compileSpec(code, streaming);
  const { spec, pruned, adopted } = normalize(loose, !streaming);
  const rootEl = spec ? spec.elements[spec.root] : undefined;
  const renderable = !!spec && rootEl?.type === ROOT && array(rootEl.children).length > 0;
  const issues: string[] = [];
  if (bad) issues.push(`${bad} line(s) are not a JSON patch`);
  if (!spec) issues.push("no root element");
  else if (rootEl?.type !== ROOT) issues.push(`the root must be a ${ROOT}`);
  else if (!array(rootEl.children).length) issues.push("the root card has no children");
  if (pruned) issues.push("an element is not in the catalog, or a child is missing");
  if (spec && !pruned) {
    const shape = catalog.validate(spec);
    if (!shape.success) {
      const found = (shape as { error?: { issues?: { path?: unknown[]; message?: string }[] } }).error?.issues ?? [];
      issues.push(...(found.length ? found.slice(0, 5).map((i) => `${array(i.path).join("/")}: ${i.message ?? "invalid"}`) : ["does not match the catalog"]));
    }
    issues.push(...propIssues(spec));
    const structure = validateSpec(spec);
    if (!structure.valid) issues.push(...structure.issues.filter((i) => i.severity !== "warning").slice(0, 5).map((i) => i.message));
  }
  const state = spec && record((spec as { state?: unknown }).state) ? (spec as { state?: Record<string, unknown> }).state! : {};
  return {
    code,
    spec,
    text: readableSpec(spec),
    renderable,
    complete: renderable && issues.length === 0,
    hasControls: Object.keys(state).length > 0 ||
      (!!spec && Object.values(spec.elements).some((el) => CONTROLS.includes(String(el.type)))),
    issues,
    adopted,
  };
}

/** Streaming may show what has arrived so far. A final reply must be whole. */
export function selectSpec(current: SpecProgram, previous: SpecProgram | undefined, streaming: boolean): SpecProgram | undefined {
  if (streaming) return current.renderable ? current : previous;
  return current.complete ? current : undefined;
}
