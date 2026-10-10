import { describe, expect, it, vi } from "vitest";
import {
  applyInsets,
  atBottom,
  BOTTOM_VAR,
  sanitizeInsets,
  SETTLE_MS,
  TOP_VAR,
  type InsetTarget,
  type ScrollBox,
} from "../insets";
import { createBridge } from "../nativeBridge";

/** A list whose content is `content` px tall in a `view` px window, scrolled to `top`. */
function list(content: number, view: number, top: number): ScrollBox {
  return { scrollTop: top, scrollHeight: content, clientHeight: view };
}

/** A page whose padding grows the list's content by the bottom inset, as the CSS does. */
function page(box: ScrollBox | null) {
  const vars = new Map<string, string>();
  const frames: (() => void)[] = [];
  const timers: { fn: () => void; ms: number }[] = [];
  const target: InsetTarget = {
    setVar: (name, value) => vars.set(name, value),
    box: () => box,
    frame: (fn) => frames.push(fn),
    later: (fn, ms) => timers.push({ fn, ms }),
  };
  const run = () => {
    frames.splice(0).forEach((f) => f());
    timers.splice(0).forEach((t) => t.fn());
  };
  return { vars, frames, timers, target, run };
}

describe("setInsets on the bridge", () => {
  it("hands valid insets to listeners, once per change, and replays the last to a late listener", () => {
    const bridge = createBridge(() => {});
    const heard = vi.fn();
    const off = bridge.onInsets(heard);
    bridge.native.setInsets({ top: 103, bottom: 92.25 });
    bridge.native.setInsets({ top: 103, bottom: 92.25 }); // same again: nothing new
    bridge.native.setInsets({ top: 103, bottom: 380 }); // the keyboard came up
    expect(heard.mock.calls).toEqual([[{ top: 103, bottom: 92.3 }], [{ top: 103, bottom: 380 }]]);

    const late = vi.fn();
    bridge.onInsets(late);
    expect(late).toHaveBeenCalledWith({ top: 103, bottom: 380 });

    off();
    bridge.native.setInsets({ top: 103, bottom: 92 });
    expect(heard).toHaveBeenCalledTimes(2);
    expect(late).toHaveBeenCalledTimes(2);
  });

  it("drops anything that is not two finite numbers, and clamps negatives to 0", () => {
    const bridge = createBridge(() => {});
    const heard = vi.fn();
    bridge.onInsets(heard);
    for (const bad of [null, undefined, 3, "x", {}, { top: 1 }, { top: "1", bottom: 2 }, { top: NaN, bottom: 1 },
      { top: 1, bottom: Infinity }]) {
      bridge.native.setInsets(bad as never);
    }
    expect(heard).not.toHaveBeenCalled();
    expect(sanitizeInsets({ top: -5, bottom: 20 })).toEqual({ top: 0, bottom: 20 });
  });
});

describe("pinned to the bottom", () => {
  it("counts the end of the list, within the slack, as the bottom", () => {
    expect(atBottom(list(2000, 800, 1200))).toBe(true);
    expect(atBottom(list(2000, 800, 1180))).toBe(true);
    expect(atBottom(list(2000, 800, 1100))).toBe(false);
    expect(atBottom(list(300, 800, 0))).toBe(true); // nothing to scroll
  });

  it("sets both padding variables in px", () => {
    const p = page(list(2000, 800, 0));
    applyInsets({ top: 103, bottom: 92 }, p.target);
    expect(p.vars.get(TOP_VAR)).toBe("103px");
    expect(p.vars.get(BOTTOM_VAR)).toBe("92px");
  });

  it("keeps a list that was at the bottom there when the keyboard grows the inset, after its animation too", () => {
    const box = list(2000, 800, 1200); // at the bottom
    const p = page(box);
    const pinned = applyInsets({ top: 103, bottom: 380 }, p.target);
    expect(pinned).toBe(true);
    // The new padding makes the content 288 px taller (380 - 92): without the pin it would sit under the composer.
    (box as { scrollHeight: number }).scrollHeight = 2288;
    expect(p.timers.map((t) => t.ms)).toEqual([SETTLE_MS]);
    p.run();
    expect(box.scrollTop).toBe(2288 - 800);
    expect(atBottom(box)).toBe(true);
  });

  it("leaves a list the owner scrolled up where it is", () => {
    const box = list(2000, 800, 400);
    const p = page(box);
    expect(applyInsets({ top: 103, bottom: 380 }, p.target)).toBe(false);
    (box as { scrollHeight: number }).scrollHeight = 2288;
    p.run();
    expect(box.scrollTop).toBe(400);
    expect(p.frames).toHaveLength(0);
    expect(p.timers).toHaveLength(0);
  });

  it("measures 'at the bottom' before the new padding, not after", () => {
    // At the bottom with the keyboard up; it goes down, and the content shrinks under the scroll position.
    const box = list(2288, 800, 1488);
    const p = page(box);
    expect(applyInsets({ top: 103, bottom: 92 }, p.target)).toBe(true);
    (box as { scrollHeight: number }).scrollHeight = 2000;
    p.run();
    expect(box.scrollTop).toBe(1200);
  });

  it("does nothing to scroll when the list is not on the page yet", () => {
    const p = page(null);
    expect(() => {
      applyInsets({ top: 50, bottom: 60 }, p.target);
      p.run();
    }).not.toThrow();
    expect(p.vars.get(BOTTOM_VAR)).toBe("60px");
  });
});
