// ChatStorage for eve's one continuous conversation, plus the long poll
// that keeps it live while the page is visible.

import type { ChatStorage, Thread } from "@openuidev/react-headless";
import type { Bridge, MessagesPayload } from "./nativeBridge";
import { reactions as pageReactions, type ReactionStore } from "./reactions";
import { Ledger, mapLines } from "./sync";

export const EVE_THREAD: Thread = { id: "eve", title: "eve", createdAt: 0 };

export interface Cursor {
  value: string | undefined;
}

const realSleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** The long poll's wait while a turn is in flight, so its end shows at once. */
export const BUSY_WAIT_S = 2;
/** The long poll's wait otherwise. */
export const IDLE_WAIT_S = 25;

export function createEveStorage(
  bridge: Bridge,
  ledger: Ledger,
  cursor: Cursor,
  onHistory: (payload: MessagesPayload) => void,
  sleep: (ms: number) => Promise<void> = realSleep,
  reactions: ReactionStore = pageReactions,
): ChatStorage {
  return {
    thread: {
      async listThreads() {
        return { threads: [EVE_THREAD] };
      },
      async createThread() {
        return EVE_THREAD;
      },
      async getMessages() {
        // Retried here with backoff rather than failed to the store: a failure
        // there swaps the list for OpenUI's error callout and a reload for its
        // loader. The thread stays empty and quiet until eve answers.
        let backoff = 1000;
        let payload: MessagesPayload;
        for (;;) {
          try {
            payload = await bridge.requestMessages(undefined, 0);
            break;
          } catch {
            await sleep(backoff);
            backoff = Math.min(backoff * 2, 15000);
          }
        }
        ledger.markSeen(payload.messages);
        reactions.ingest(payload);
        cursor.value = payload.cursor;
        onHistory(payload);
        return mapLines(payload.messages);
      },
      async updateThread(thread) {
        return thread;
      },
      async deleteThread() {
        // one conversation; nothing to delete
      },
    },
  };
}

export interface PollOptions {
  bridge: Bridge;
  ledger: Ledger;
  cursor: Cursor;
  onBusy: (busy: boolean) => void;
  isVisible: () => boolean;
  sleep?: (ms: number) => Promise<void>;
  /** Where the reactions each answer carries go (reactions.ts). */
  reactions?: ReactionStore;
}

/**
 * Long-polls /api/link/messages while the page is visible. Returns a stop
 * function. Call `kick()` when the page becomes visible again.
 */
export function startLivePoll(opts: PollOptions): { stop: () => void; kick: () => void } {
  const sleep = opts.sleep ?? realSleep;
  let stopped = false;
  let busy = false;
  let running = false;
  let backoff = 1000;

  const loop = async () => {
    if (running) return;
    running = true;
    try {
      while (!stopped && opts.isVisible()) {
        if (opts.cursor.value === undefined) {
          await sleep(250);
          continue;
        }
        try {
          // While busy, a short wait: the daemon wakes a long poll on a new
          // line, and eve's last line is written while the turn is still in
          // flight, so a 25 s wait kept the "replying" dots up that long.
          const payload = await opts.bridge.requestMessages(opts.cursor.value, busy ? BUSY_WAIT_S : IDLE_WAIT_S);
          if (stopped) break;
          if (payload.cursor) opts.cursor.value = payload.cursor;
          opts.ledger.ingest(payload.messages);
          (opts.reactions ?? pageReactions).ingest(payload);
          busy = payload.busy;
          opts.onBusy(payload.busy);
          backoff = 1000;
        } catch {
          await sleep(backoff);
          backoff = Math.min(backoff * 2, 15000);
        }
      }
    } finally {
      running = false;
    }
  };

  void loop();
  return {
    stop: () => {
      stopped = true;
    },
    kick: () => {
      if (!stopped) void loop();
    },
  };
}
