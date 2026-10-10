import { describe, expect, it } from "vitest";
import { LIVE_MARK, ownerText, splitReply, wrapAction } from "../replyParts";
import { mapLines, ownerContent } from "../sync";

describe("splitReply", () => {
  it("plain markdown stays one markdown part", () => {
    expect(splitReply("**Done.** Lunch at 1.\n\n- a\n- b", false)).toEqual([
      { kind: "markdown", text: "**Done.** Lunch at 1.\n\n- a\n- b" },
    ]);
  });

  it("prose that mentions root = 5 is still markdown", () => {
    expect(splitReply("Set the root = 5 and retry.\nroot = 5", false)[0]?.kind).toBe("markdown");
  });

  it("a fenced openui-lang block becomes a ui part between markdown parts", () => {
    const text = 'Here you go:\n```openui-lang\nroot = Card([t])\nt = TextContent("hi")\n```\nAnything else?';
    expect(splitReply(text, false)).toEqual([
      { kind: "markdown", text: "Here you go:" },
      { kind: "ui", code: 'root = Card([t])\nt = TextContent("hi")', complete: true },
      { kind: "markdown", text: "Anything else?" },
    ]);
  });

  it("an unclosed fence while streaming is an incomplete ui part", () => {
    expect(splitReply("A table:\n```openui-lang\nroot = Card([", true)).toEqual([
      { kind: "markdown", text: "A table:" },
      { kind: "ui", code: "root = Card([", complete: false },
    ]);
  });

  it("hides a half-arrived fence opener while streaming", () => {
    expect(splitReply("Sure.\n```openui-l", true)).toEqual([{ kind: "markdown", text: "Sure." }]);
  });

  it("other code fences stay markdown", () => {
    const text = "```python\nroot = Tk()\n```";
    expect(splitReply(text, false)).toEqual([{ kind: "markdown", text }]);
  });

  it("a bare program (no fence) renders as ui", () => {
    const text = 'root = Card([t])\nt = TextContent("hi")';
    expect(splitReply(text, false)).toEqual([{ kind: "ui", code: text, complete: true }]);
  });
});

describe("ownerText", () => {
  it("plain text goes as typed, trimmed", () => {
    expect(ownerText("  hello  ")).toBe("hello");
  });

  it("a component action sends the human text and its context", () => {
    const content = wrapAction("Book the 9am", ["User clicked: Book the 9am", { slot: { value: "9" } }]);
    expect(ownerText(content)).toBe('Book the 9am\n\n["User clicked: Book the 9am",{"slot":{"value":"9"}}]');
  });

  it("multipart content joins its text parts", () => {
    expect(ownerText([{ type: "text", text: "a" }, { type: "text", text: "b" }])).toBe("a\nb");
  });
});

describe("history: an owner's component action", () => {
  it("comes back as the human text with its context, never raw JSON", () => {
    const live = wrapAction("Yes", ["User clicked: Yes", { choice: { value: "yes" } }]);
    const transcript = ownerText(live); // what the daemon stored
    expect(transcript).toBe('Yes\n\n["User clicked: Yes",{"choice":{"value":"yes"}}]');
    expect(ownerContent(transcript)).toBe(live);
    const [msg] = mapLines([{ id: "d:1", role: "owner", text: transcript, t: 1, via: "app", kind: "say" }]);
    expect(msg?.content).toBe(live);
  });

  it("leaves typed text alone, brackets and all", () => {
    expect(ownerContent("hi")).toBe("hi");
    expect(ownerContent('see\n\n["not a click"]')).toBe('see\n\n["not a click"]');
    expect(ownerContent('x\n\n["User clicked: broken json')).toBe('x\n\n["User clicked: broken json');
  });

  it("drops the live mark the page puts in front of a streaming reply", () => {
    expect(splitReply(LIVE_MARK, true)).toEqual([]);
    expect(splitReply(`${LIVE_MARK}Hi`, true)).toEqual([{ kind: "markdown", text: "Hi" }]);
  });
});
