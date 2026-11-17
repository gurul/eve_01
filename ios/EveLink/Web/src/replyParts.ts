// How one assistant message splits into what to render.
//
// eve answers in plain markdown most of the time. When a table, chart,
// list, steps or choices help, it writes OpenUI Lang in a ```openui-lang
// fence (the generated prompt asks for that). Text outside a fence is
// markdown. An unfenced reply that starts a program with `root =` is OpenUI
// Lang as a whole (the library's own convention).
//
// With the json-render engine on (the bridge's EVE_CARD_ENGINE), the card is
// a ```json-render fence instead: SpecStream lines, drawn by JsonCard.tsx.
// The page reads both fences whatever the switch says, so the history of one
// engine still draws after a change to the other.

export type ReplyPart =
  | { kind: "markdown"; text: string }
  | { kind: "ui"; code: string; complete: boolean }
  | { kind: "spec"; code: string; complete: boolean };

const FENCE = "```openui-lang";
const SPEC_FENCE = "```json-render";
const FENCES = [FENCE, SPEC_FENCE];
/** The first card fence in `text`: where it opens and which engine it is for. */
function nextFence(text: string): { at: number; kind: "ui" | "spec" } | undefined {
  const ui = text.indexOf(FENCE);
  const spec = text.indexOf(SPEC_FENCE);
  if (ui === -1 && spec === -1) return undefined;
  return spec === -1 || (ui !== -1 && ui < spec) ? { at: ui, kind: "ui" } : { at: spec, kind: "spec" };
}
// `root = Card(` at the start of a line: a program, not prose that says "root = 5".
const ROOT_RE = /(^|\n)[ \t]*root[ \t]*=[ \t]*[A-Z][A-Za-z0-9]*\(/;

/** Drop a half-typed fence opener at the very end of a streaming reply. */
function trimPartialFence(text: string): string {
  const nl = text.lastIndexOf("\n");
  const tail = text.slice(nl + 1);
  if (tail.startsWith("`") && FENCES.some((f) => f.startsWith(tail))) return text.slice(0, nl + 1);
  return text;
}

function pushMarkdown(parts: ReplyPart[], text: string): void {
  if (text.trim()) parts.push({ kind: "markdown", text: text.replace(/^\n+|\n+$/g, "") });
}

/**
 * An empty openui-lang fence the page puts in front of a live reply
 * (llm.ts liveMarkdownAdapter) so OpenUI shows a markdown reply while it
 * streams. Never part of what eve said: splitReply drops it.
 */
export const LIVE_MARK = "```openui-lang\n```\n";

export function splitReply(raw: string, streaming: boolean): ReplyPart[] {
  const text = (raw ?? "").startsWith(LIVE_MARK) ? raw.slice(LIVE_MARK.length) : (raw ?? "");
  const parts: ReplyPart[] = [];

  if (!nextFence(text)) {
    if (ROOT_RE.test(text) && !/```/.test(text.slice(0, text.search(ROOT_RE) + 1))) {
      // A bare program: everything before `root =` that is prose stays prose.
      const at = text.search(ROOT_RE);
      pushMarkdown(parts, text.slice(0, at));
      parts.push({ kind: "ui", code: text.slice(at).replace(/^\n/, ""), complete: !streaming });
      return parts;
    }
    pushMarkdown(parts, streaming ? trimPartialFence(text) : text);
    return parts;
  }

  let rest = text;
  while (rest.length) {
    const fence = nextFence(rest);
    if (!fence) {
      pushMarkdown(parts, streaming ? trimPartialFence(rest) : rest);
      break;
    }
    const open = fence.at;
    const kind = fence.kind;
    pushMarkdown(parts, rest.slice(0, open));
    const bodyStart = rest.indexOf("\n", open);
    if (bodyStart === -1) {
      // Only the opener has arrived.
      parts.push({ kind, code: "", complete: false });
      break;
    }
    const close = rest.indexOf("\n```", bodyStart);
    if (close === -1) {
      parts.push({ kind, code: rest.slice(bodyStart + 1), complete: false });
      break;
    }
    parts.push({ kind, code: rest.slice(bodyStart + 1, close), complete: true });
    const after = rest.indexOf("\n", close + 4);
    rest = after === -1 ? "" : rest.slice(after + 1);
  }
  return parts;
}

// OpenUI's inline sentinels (react-ui utils/sentinelParser, not exported).
// A component action ("continue the conversation") sends a user message
// whose content is `]]>openui:content\n<human text>` plus, optionally,
// `\n]]>openui:context\n<json>` with what was clicked and the form values.
const CONTENT_MARKER = "]]>openui:content";
const CONTEXT_MARKER = "]]>openui:context";

export function wrapAction(human: string, context: unknown[]): string {
  return `${CONTENT_MARKER}\n${human}\n${CONTEXT_MARKER}\n${JSON.stringify(context)}`;
}

/**
 * The text a user message sends to the daemon. Plain text goes as typed. A
 * component action goes as its human text plus the context JSON, so the
 * brain sees what was clicked and the form values.
 */
export function ownerText(content: unknown): string {
  const raw =
    typeof content === "string"
      ? content
      : Array.isArray(content)
        ? content
            .map((p) => (p && typeof p === "object" && "text" in p ? String((p as { text: unknown }).text) : ""))
            .filter((t) => t !== "")
            .join("\n")
        : "";
  if (!raw.includes(CONTENT_MARKER) && !raw.includes(CONTEXT_MARKER)) return raw.trim();
  const ci = raw.lastIndexOf(CONTENT_MARKER);
  const xi = raw.lastIndexOf(CONTEXT_MARKER);
  const bodyAfter = (i: number) => {
    const nl = raw.indexOf("\n", i);
    return nl === -1 ? raw.length : nl + 1;
  };
  let human = "";
  let context = "";
  if (ci !== -1) human = raw.slice(bodyAfter(ci), xi > ci ? xi : raw.length);
  if (xi !== -1) context = raw.slice(bodyAfter(xi));
  human = human.trim();
  context = context.trim();
  return context ? `${human}\n\n${context}`.trim() : human;
}
