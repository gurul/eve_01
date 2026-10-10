// Where the native chrome covers the page (docs/app-chat.md, "The cut-off").
//
// The web view fills the whole screen, under the native top bar and the
// native composer. Native measures both and calls
// window.eveNative.setInsets({top, bottom}) in CSS px on every change: top
// is the top bar's bottom edge, bottom is the distance from the composer's
// top edge (on the keyboard, when it is up) to the bottom of the web view.
// The page pads the message list by them, so the first message starts below
// the bar and the last one sits 16 px above the composer. A list that was at
// the bottom stays pinned there when the inset changes.

export interface Insets {
  top: number;
  bottom: number;
}

/** A scrolling element, or a fake one in tests. */
export interface ScrollBox {
  scrollTop: number;
  readonly scrollHeight: number;
  readonly clientHeight: number;
}

/** What applyInsets needs from the page, so tests can give fakes. */
export interface InsetTarget {
  setVar(name: string, value: string): void;
  box(): ScrollBox | null;
  /** Run after the next layout (requestAnimationFrame). */
  frame(fn: () => void): void;
  /** Run after `ms` (setTimeout). */
  later(fn: () => void, ms: number): void;
}

export const TOP_VAR = "--eve-inset-top";
export const BOTTOM_VAR = "--eve-inset-bottom";
/** Within this many px of the end counts as "at the bottom". */
export const PIN_SLACK = 24;
/** The keyboard animates for about 250 ms: pin again once it has settled. */
export const SETTLE_MS = 320;
/** Larger than any screen: anything above it is a bad call. */
const MAX_INSET = 4000;

/** The insets native sent, or null when they are not two finite numbers. Negative values are 0. */
export function sanitizeInsets(raw: unknown): Insets | null {
  if (!raw || typeof raw !== "object") return null;
  const { top, bottom } = raw as { top?: unknown; bottom?: unknown };
  if (typeof top !== "number" || typeof bottom !== "number") return null;
  if (!Number.isFinite(top) || !Number.isFinite(bottom)) return null;
  const clamp = (v: number) => Math.round(Math.min(MAX_INSET, Math.max(0, v)) * 10) / 10;
  return { top: clamp(top), bottom: clamp(bottom) };
}

export function sameInsets(a: Insets | null, b: Insets | null): boolean {
  return !!a && !!b && a.top === b.top && a.bottom === b.bottom;
}

/** The list shows its end (or has nothing to scroll). */
export function atBottom(box: ScrollBox, slack = PIN_SLACK): boolean {
  return box.scrollHeight - box.scrollTop - box.clientHeight <= slack;
}

/**
 * Set the padding variables. If the list was at the bottom before, keep it
 * there: once after the new padding is laid out, and once more after the
 * keyboard's animation. Returns whether it pinned.
 */
export function applyInsets(insets: Insets, target: InsetTarget): boolean {
  const before = target.box();
  const pinned = !before || atBottom(before);
  target.setVar(TOP_VAR, `${insets.top}px`);
  target.setVar(BOTTOM_VAR, `${insets.bottom}px`);
  if (pinned) {
    const pin = () => {
      const box = target.box();
      if (box) box.scrollTop = Math.max(0, box.scrollHeight - box.clientHeight);
    };
    target.frame(pin);
    target.later(pin, SETTLE_MS);
  }
  return pinned;
}

/** The real page: CSS variables on <html>, the OpenUI scroll area. */
export function domInsetTarget(selector: string): InsetTarget {
  return {
    setVar: (name, value) => document.documentElement.style.setProperty(name, value),
    box: () => document.querySelector<HTMLElement>(selector),
    frame: (fn) => requestAnimationFrame(() => fn()),
    later: (fn, ms) => setTimeout(fn, ms),
  };
}
