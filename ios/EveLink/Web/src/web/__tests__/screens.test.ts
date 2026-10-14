import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readOnboard } from "../api";
import { SignIn } from "../Gate";
import { MAC_SIGNED_OUT } from "../host";
import { ADD_CREDITS } from "../Settings";
import { Setup } from "../Setup";
import { draw, item, link, text } from "./markup";

/** Setup with OpenRouter connected, and `credit` as the Mac's /onboard/state passes on onboarding.credit(). */
const setup = (credit: unknown) =>
  draw(Setup, { state: readOnboard({ openrouter: true, credit, composio: false }), onChanged: () => {} });

beforeEach(() => {
  // Setup names the app as OpenRouter's consent page does: this page's host.
  vi.stubGlobal("window", { location: { host: "localhost:48780" } });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("setup's OpenRouter step", () => {
  it("usd null and funded false (an OAuth key on an account that never bought credits): no credit yet, Add credits in a new tab, and Check again", () => {
    const html = setup({ ok: true, usd: null, funded: false });
    const add = link(html, "Add credits");
    expect(add, "no Add credits link").not.toBe("");
    expect(add).toContain(`href="${ADD_CREDITS}"`);
    expect(add).toContain('target="_blank"');
    expect(add).toContain('rel="noopener noreferrer"');
    expect(item(html, 1)).toMatch(/^OpenRouter OpenRouter connected · no credit yet .*Add credits Check again$/);
  });

  it("at $0 (a figure, or a bare number): the figure, Add credits and Check again", () => {
    for (const credit of [{ ok: true, usd: 0, funded: false }, 0]) {
      const html = setup(credit);
      expect(link(html, "Add credits")).toContain(`href="${ADD_CREDITS}"`);
      expect(item(html, 1)).toMatch(/^OpenRouter OpenRouter connected · \$0\.00 credit .*Add credits Check again$/);
    }
  });

  it("a funded key shows its figure and a key not known shows none, neither with Add credits", () => {
    expect(item(setup({ ok: true, usd: 4.97, funded: true }), 1)).toBe("OpenRouter OpenRouter connected · $4.97 credit");
    expect(item(setup({ ok: true, usd: null, funded: null }), 1)).toBe("OpenRouter OpenRouter connected");
    expect(item(setup({ ok: false, usd: null, funded: null, error: "HTTP 401" }), 1)).toBe(
      "OpenRouter OpenRouter connected",
    );
  });

  it("a turned-down key: step 1 is not done and offers a new sign-in, with no Add credits", () => {
    const html = draw(Setup, {
      state: readOnboard({ openrouter: true, openrouter_rejected: true, composio: false }),
      onChanged: () => {},
    });
    expect(link(html, "Add credits")).toBe("");
    const step = item(html, 1);
    expect(step).toContain("stopped working");
    expect(step).toContain("Sign in with OpenRouter again");
  });
});

const FULL = "~/.eve-lite/venv/bin/eve-bridge web open";

/** eve-bridge named anywhere but inside the full command: a command a lite Mac would not find on its PATH. */
const bare = (words: string) => words.split(FULL).some((part) => part.includes("eve-bridge"));

describe("the Mac's sign-in screen", () => {
  it("names Eve, and the venv's full eve-bridge web open for a Mac without it, never a bare eve-bridge", () => {
    expect(bare("or run eve-bridge web open in Terminal")).toBe(true); // control: the check sees a bare one
    expect(bare(`Paste this line: ${FULL}`)).toBe(false);
    const words = text(draw(SignIn, { note: "", onRetry: () => {} }));
    expect(bare(words), "a bare eve-bridge on the sign-in screen").toBe(false);
    expect(words).toContain(`Terminal and press Return: ${FULL} Copy`);
    expect(words).toContain("Open Eve from Applications");
  });

  it("says MAC_SIGNED_OUT once, in its own words, and shows any other note", () => {
    const signedOut = text(draw(SignIn, { note: MAC_SIGNED_OUT, onRetry: () => {} }));
    expect(signedOut.split("This browser isn't signed in to eve.").length - 1, "the signed-out line twice").toBe(1);
    const used = "That sign-in link was already used. Open Eve from Applications again.";
    expect(text(draw(SignIn, { note: used, onRetry: () => {} }))).toContain(used);
  });
});
