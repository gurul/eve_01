import { describe, expect, it } from "vitest";
import { compactLinks, linkLabel } from "../links";

describe("compactLinks", () => {
  it("turns a bare Google Maps URL into a short Maps label", () => {
    const md = "Your class is in GUG 220. https://www.google.com/maps/search/?api=1&query=GUG%20220";
    expect(compactLinks(md)).toBe(
      "Your class is in GUG 220. [Maps · GUG 220](https://www.google.com/maps/search/?api=1&query=GUG%20220)",
    );
  });

  it("labels other links by host and first path segment, and keeps trailing punctuation outside", () => {
    expect(compactLinks("see https://docs.example.com/guide/setup.")).toBe(
      "see [docs.example.com › guide](https://docs.example.com/guide/setup).",
    );
  });

  it("leaves markdown links, angle links and code alone", () => {
    const md = "[here](https://a.example.com/x) <https://b.example.com> `https://c.example.com` ```\nhttps://d.example.com\n```";
    expect(compactLinks(md)).toBe(md);
  });

  it("positive control: the same URL outside code is rewritten", () => {
    expect(compactLinks("https://c.example.com")).toBe("[c.example.com](https://c.example.com)");
  });

  it("survives a malformed URL", () => {
    expect(linkLabel("https://")).toBe("https://");
  });

  it("survives a path segment that is not valid percent-encoding", () => {
    // decodeURIComponent("50%off") throws URIError, which blanked the whole chat.
    expect(linkLabel("https://shop.com/50%off")).toBe("shop.com › 50%off");
    expect(compactLinks("see https://shop.com/50%off")).toBe("see [shop.com › 50%off](https://shop.com/50%off)");
  });

  it("positive control: a valid escape in the first segment is still decoded", () => {
    expect(linkLabel("https://shop.com/caf%C3%A9")).toBe("shop.com › café");
  });
});
