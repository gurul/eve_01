// This browser's own small choices, kept in localStorage when it can be (never a secret, and never needed: the
// page works the same with storage blocked, as in a private window).

import type { Grade } from "./host";

const GRADE_KEY = "eve.grade";

function read(key: string): string | null {
  try {
    return globalThis.localStorage?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  try {
    globalThis.localStorage?.setItem(key, value);
  } catch {
    // storage blocked: the choice lasts this visit
  }
}

let grade: Grade | null = null; // read at first use, not at import
const listeners = new Set<() => void>();

/** The composer's Think toggle: "think" runs the next turns on the app's think model, "auto" as eve decides. */
export const gradePref = {
  get: (): Grade => (grade ??= read(GRADE_KEY) === "think" ? "think" : "auto"),
  set(next: Grade): void {
    if (next === gradePref.get()) return;
    grade = next;
    write(GRADE_KEY, next);
    for (const listener of [...listeners]) listener();
  },
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
};
