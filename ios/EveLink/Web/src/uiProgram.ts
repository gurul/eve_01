// Parse against the same catalog as the renderer. No generated code is executed
// here: the text view reads only display fields from resolved component nodes.
import { createParser, type ElementNode, type ParseResult } from "@openuidev/react-lang";
import { openuiChatLibrary } from "@openuidev/react-ui/genui-lib";

const parser = createParser(openuiChatLibrary.toJSONSchema(), openuiChatLibrary.root);
const scalar = (value: unknown): string =>
  typeof value === "string" || typeof value === "number" || typeof value === "boolean" ? String(value) : "";
const array = (value: unknown): unknown[] => Array.isArray(value) ? value : [];

function element(value: unknown): value is ElementNode {
  return !!value && typeof value === "object" && "type" in value && value.type === "element";
}

/** Static, selectable content; never serializes actions, field IDs or runtime ASTs. */
export function readableUi(root: ElementNode | null): string {
  function read(value: unknown, depth = 0): string[] {
    if (depth > 40) return [];
    if (Array.isArray(value)) return value.flatMap((v) => read(v, depth + 1));
    if (!element(value)) return [];
    const p = value.props;
    const text = (...keys: string[]) => keys.map((k) => scalar(p[k])).filter(Boolean);
    const children = (...keys: string[]) => keys.flatMap((k) => read(p[k], depth + 1));
    switch (value.typeName) {
      case "Table": {
        const cols = array(p.columns).filter(element);
        const count = Math.max(0, ...cols.map((col) => array(col.props.data).length));
        return Array.from({ length: count }, (_, i) => cols.map((col) => {
          const cell = array(col.props.data)[i];
          const content = element(cell) ? read(cell, depth + 1).join(" ") : scalar(cell);
          return `${scalar(col.props.label)}: ${content || "—"}`;
        }).join(" · "));
      }
      case "BarChart":
      case "LineChart": {
        const labels = array(p.labels);
        return [...text("xLabel", "yLabel"), ...array(p.series).filter(element).flatMap((series) =>
          array(series.props.values).map((v, i) =>
            [scalar(labels[i]), scalar(series.props.category), scalar(v)].filter(Boolean).join(" · ")),
        )];
      }
      case "PieChart":
        return array(p.values).map((v, i) => `${scalar(array(p.labels)[i])}: ${scalar(v)}`);
      case "EntityList":
        return [p.header, ...array(p.rows), p.footer].flatMap((row) => {
          if (!row || typeof row !== "object") return [];
          const r = row as Record<string, unknown>;
          return [[scalar(r.left), scalar(r.right)].filter(Boolean).join(": ")];
        });
      case "CardHeader": return text("title", "subtitle");
      case "TextContent":
      case "MarkDownRenderer":
      case "FollowUpItem": return text("text", "textMarkdown");
      case "StepsItem": return text("title", "details");
      case "ListItem": return text("title", "subtitle");
      case "Callout": return text("title", "description");
      case "ImageBlock":
      case "Image": return text("alt");
      case "FormControl": return [...text("label", "hint"), ...children("input")];
      case "Input":
      case "TextArea": return p.type === "password" ? [] : text("placeholder");
      case "Button":
      case "SelectItem":
      case "ChipItem":
      case "Tag": return text("label", "text");
      case "TabItem":
      case "AccordionItem": return [...text("trigger"), ...children("content")];
      default: return children("children", "items", "fields", "buttons", "content");
    }
  }
  return read(root).filter(Boolean).join("\n\n");
}

function hasInputs(value: unknown): boolean {
  if (Array.isArray(value)) return value.some(hasInputs);
  if (!element(value)) return false;
  if (["Input", "TextArea", "Select", "Chips", "Slider", "DatePicker", "CheckBoxGroup", "RadioGroup", "OptionCards"].includes(value.typeName)) return true;
  return Object.values(value.props).some(hasInputs);
}

export interface UiProgram {
  code: string;
  text: string;
  renderable: boolean;
  complete: boolean;
  hasControls: boolean;
}

export function inspectUi(code: string): UiProgram {
  let result: ParseResult;
  try {
    result = parser.parse(code);
  } catch {
    return { code, text: "", renderable: false, complete: false, hasControls: false };
  }
  const root = result.root;
  // A warning (a Card entry not named `root`) still renders; only errors stop a card.
  const errors = result.meta.errors.filter((e) => e.severity !== "warning");
  const renderable = !!root && root.typeName === openuiChatLibrary.root &&
    array(root.props.children).some(element) && errors.length === 0 &&
    result.queryStatements.length === 0 && result.mutationStatements.length === 0;
  return {
    code,
    text: readableUi(root),
    renderable,
    hasControls: hasInputs(root) || Object.keys(result.stateDeclarations).length > 0,
    complete: renderable && !result.meta.incomplete && result.meta.unresolved.length === 0,
  };
}

/** Streaming may recover unfinished syntax. A final reply must be whole. */
export function selectUi(current: UiProgram, previous: UiProgram | undefined, streaming: boolean): UiProgram | undefined {
  if (streaming) return current.renderable ? current : previous;
  return current.complete ? current : undefined;
}

/** A bounded, page-local cache. Never writes form contents to disk or a log. */
export class UiStateCache {
  private states = new Map<string, Record<string, unknown>>();
  constructor(private limit = 100) {}
  get(key: string): Record<string, unknown> | undefined { return this.states.get(key); }
  delete(key: string): void { this.states.delete(key); }
  set(key: string, state: Record<string, unknown>): void {
    this.states.delete(key);
    this.states.set(key, state);
    while (this.states.size > this.limit) this.states.delete(this.states.keys().next().value!);
  }
}
