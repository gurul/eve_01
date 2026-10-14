import { describe, expect, it } from "vitest";
import { readStatus } from "../api";
import { ADD_CREDITS, money, OpenRouter } from "../Settings";
import { draw, link, text } from "./markup";

/** Settings' OpenRouter card for `credit` as the Mac's /api/web/status passes on onboarding.credit(). */
const card = (credit: unknown) =>
  draw(OpenRouter, { status: readStatus({ credit }), reload: () => {}, where: "mac" });
const LEAD = "OpenRouter eve's models, voice and search run on your OpenRouter key.";

describe("Settings' OpenRouter card", () => {
  it("says no credit yet for usd null and funded false, no credit left at $0, the figure when funded, nothing when not known", () => {
    expect(text(card({ ok: true, usd: null, funded: false }))).toBe(`${LEAD} It has no credit yet. Add credits Check again`);
    expect(text(card({ ok: true, usd: 0, funded: false }))).toBe(`${LEAD} It has no credit left. Add credits Check again`);
    expect(text(card({ ok: true, usd: 4.97, funded: true }))).toBe(`${LEAD} It has $4.97 of credit. Add credits Check again`);
    expect(text(card({ ok: true, usd: null, funded: null }))).toBe(`${LEAD} Add credits Check again`);
    expect(text(draw(OpenRouter, { status: null, reload: () => {}, where: "mac" }))).toBe(
      `${LEAD} Add credits Check again`,
    );
  });

  it("a turned-down key offers a new sign-in on the Mac and points the phone there, with no Add credits", () => {
    const rejected = (where: "mac" | "lan") =>
      draw(OpenRouter, { status: readStatus({ openrouter_rejected: true, credit: { ok: false, error: "HTTP 401" } }), reload: () => {}, where });
    const onMac = rejected("mac");
    expect(text(onMac)).toContain("stopped working");
    expect(link(onMac, "Add credits")).toBe("");
    expect(onMac).toContain("Sign in with OpenRouter again");
    const onPhone = rejected("lan");
    expect(onPhone).not.toContain("Sign in with OpenRouter again"); // the sign-in is a Mac-only route
    expect(text(onPhone)).toContain("on the Mac");
  });

  it("Add credits is the main button only when there is no credit, and opens openrouter.ai in a new tab", () => {
    const main = 'class="eve-button"';
    const plain = 'class="eve-button eve-button--plain"';
    for (const [credit, kind] of [
      [{ ok: true, usd: null, funded: false }, main],
      [{ ok: true, usd: 0, funded: false }, main],
      [{ ok: true, usd: 4.97, funded: true }, plain],
      [{ ok: true, usd: null, funded: null }, plain],
    ] as const) {
      const add = link(card(credit), "Add credits");
      expect(add).toContain(kind);
      expect(add).toContain(`href="${ADD_CREDITS}"`);
      expect(add).toContain('target="_blank"');
      expect(add).toContain('rel="noopener noreferrer"');
    }
  });

  it("money: two decimals, and owing reads as $0.00", () => {
    expect([money(4.97), money(0), money(12.5), money(-1.25)]).toEqual(["$4.97", "$0.00", "$12.50", "$0.00"]);
  });
});
