// Sends the owner makes while eve is still replying (docs/app-chat.md,
// "Sending while eve replies").
//
// Owner, 2026-10-26: "I wanted to just be able to double-send or
// triple-send, and it just catches things", as Claude Code queues what is
// typed while it works. The composer stays a send button while eve replies.
// Each send that cannot start yet waits here, in order, and starts as an
// ordinary turn once the current reply has ended: finished, failed or
// stopped (so Stop ends the reply and the next one goes). The one wait
// beyond that: after a stream the app cut in the background, the next send
// starts when the page shows again (hiddenAfterError). Until it starts,
// the thread shows it at its end as a dimmed owner bubble marked "Queued"
// (QueuedMessages.tsx). The moment it starts it leaves this queue, in the same
// call that hands it to OpenUI, so it is never shown twice.
//
// A queued send never joins the reply that is running: folding a message
// into a running turn (steering) missed its eval bar on 2026-10-26.

import type { Attachment } from "./attachments";

export interface QueuedSend {
  /** The bubble's key on this page ("queued:3"); never a message id. */
  key: string;
  text: string;
  attachments: Attachment[];
}

/** What says whether the next send may start now (App.tsx reads it fresh on every try). */
export interface StartGate {
  /** A turn this page started has not settled yet (App.tsx `inFlight`). */
  inFlight: boolean;
  /** OpenUI is running a turn. */
  running: boolean;
  /** OpenUI is loading the thread. */
  loading: boolean;
  /** The first page of history has arrived. */
  historyLoaded: boolean;
  /**
   * The page is hidden and the last reply ended in an error. In the app that
   * is a stream it cut after 25 s in the background (docs/app-chat.md,
   * "Background and foreground"), and a turn started now could stay open with
   * nobody reading it. The next send waits until the page shows again.
   */
  hiddenAfterError: boolean;
}

/** A send may start only when nothing of this page's is running, the thread is loaded, and it would be read. */
export function mayStart(gate: StartGate): boolean {
  return !gate.inFlight && !gate.running && !gate.loading && gate.historyLoaded && !gate.hiddenAfterError;
}

const NONE: readonly QueuedSend[] = Object.freeze([]);

export class SendQueue {
  private items: QueuedSend[] = [];
  private view: readonly QueuedSend[] = NONE;
  private readonly listeners = new Set<() => void>();
  private made = 0;

  /** The owner's send goes to the end of the line. */
  push(text: string, attachments: Attachment[] = []): QueuedSend {
    const item: QueuedSend = { key: `queued:${++this.made}`, text, attachments };
    this.items.push(item);
    this.changed();
    return item;
  }

  /** The send to start now, taken off the queue; none while `gate` says wait or nothing waits. */
  take(gate: StartGate): QueuedSend | undefined {
    if (this.items.length === 0 || !mayStart(gate)) return undefined;
    const next = this.items.shift();
    this.changed();
    return next;
  }

  get size(): number {
    return this.items.length;
  }

  /** What waits, oldest first. The same array until the queue changes (useSyncExternalStore). */
  readonly snapshot = (): readonly QueuedSend[] => this.view;

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  private changed(): void {
    this.view = this.items.length ? Object.freeze(this.items.slice()) : NONE;
    for (const listener of this.listeners) listener();
  }
}
