// The chat page: OpenUI's AgentInterface showing ONLY the message list. The
// sidebar, headers, composer and workspace rail are emptied (the composer is
// native SwiftUI). NativeGlue connects native's calls to the OpenUI store.

import type { Message } from "@openuidev/react-headless";
import { useThread, useThreadList } from "@openuidev/react-headless";
import { AgentInterface, DotMatrixLoader, useSystemThemeMode } from "@openuidev/react-ui";
import { openuiChatLibrary } from "@openuidev/react-ui/genui-lib";
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import { EveAssistantMessage } from "./AssistantMessage";
import { ownerParts } from "./attachments";
import { EveUserMessage } from "./UserMessage";
import { HorizontalHistory } from "./HorizontalHistory";
import { QueuedMessages } from "./QueuedMessages";
import { applyInsets, domInsetTarget } from "./insets";
import { createBridgeLLM } from "./llm";
import type { Bridge } from "./nativeBridge";
import { sources, type Source } from "./sources";
import { EVE_THREAD, createEveStorage, startLivePoll, type Cursor } from "./storage";
import { Ledger, mapLines, replyAfter, separator } from "./sync";
import { reactions as pageReactions } from "./reactions";
import { SendQueue } from "./sendQueue";
import { darkTheme, lightTheme } from "./theme";

const SCROLL_AREA = ".openui-agent-thread-scroll-area";

interface Wiring {
  ledger: Ledger;
  cursor: Cursor;
  sink: {
    append: (...m: Message[]) => void;
    lastRole: () => Message["role"] | undefined;
    idle: () => boolean;
    insertBeforeReply: (...m: Message[]) => boolean;
    attachSources: (s: Source[], userId?: string) => void;
  };
  historyLoaded: { value: boolean };
}

function NativeGlue({ bridge, wiring }: { bridge: Bridge; wiring: Wiring }) {
  const selectThread = useThreadList((s) => s.selectThread);
  const switchToNewThread = useThreadList((s) => s.switchToNewThread);
  const processMessage = useThread((s) => s.processMessage);
  const appendMessages = useThread((s) => s.appendMessages);
  const deleteMessage = useThread((s) => s.deleteMessage);
  const setMessages = useThread((s) => s.setMessages);
  const messages = useThread((s) => s.messages);
  const isRunning = useThread((s) => s.isRunning);
  const isLoadingMessages = useThread((s) => s.isLoadingMessages);
  const threadError = useThread((s) => s.threadError);
  const [busy, setBusy] = useState(false);
  const [settled, setSettled] = useState(0);

  // Sends that wait for the reply on the screen to end (sendQueue.ts), shown
  // at the thread's end until each starts (QueuedMessages.tsx).
  const [queue] = useState(() => new SendQueue());
  const queued = useSyncExternalStore(queue.subscribe, queue.snapshot, queue.snapshot);
  const latest = useRef({ messages, isRunning, isLoadingMessages, threadError });
  latest.current = { messages, isRunning, isLoadingMessages, threadError };
  // A turn this page started and whose run has not settled. Set before
  // processMessage and cleared when its promise does, so two sends that
  // arrive before React re-renders never both reach processMessage (the
  // second returned early inside the store, and its words were lost).
  const inFlight = useRef(false);
  // The thread's last role, right now: refreshed on every render and on every
  // append, so two appends between renders still place their separators.
  const lastRole = useRef<Message["role"] | undefined>(undefined);
  lastRole.current = messages.at(-1)?.role;

  // The ledger appends through the store's functional append only, so a
  // stale read here can never replace newer messages. A line from the long
  // poll scrolls into view when the owner is already at the bottom, as in a
  // native chat; scrolled up, they keep their place (ScrollToLatest shows).
  wiring.sink.append = (...m) => {
    const el = document.querySelector<HTMLElement>(SCROLL_AREA);
    const near = !!el && el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    appendMessages(...m);
    lastRole.current = m.at(-1)?.role ?? lastRole.current;
    if (m.some((x) => x.role === "assistant")) bridge.post({ type: "haptic", style: "soft" });
    if (near && el) requestAnimationFrame(() => el.scrollTo({ top: el.scrollHeight, behavior: "smooth" }));
  };
  wiring.sink.lastRole = () => lastRole.current;
  // A picture eve sent mid-turn goes before the page's streamed reply, as
  // in the transcript (sync.ts). The reply is taken out and put back after
  // it, both through the store's functional updates, never a replace.
  wiring.sink.insertBeforeReply = (...m) => {
    const thread = latest.current.messages;
    const reply = thread.at(-1);
    if (!reply || reply.role !== "assistant" || !thread.some((x) => x.role === "user")) return false;
    deleteMessage(reply.id);
    wiring.sink.append(...m, reply);
    return true;
  };
  wiring.sink.idle = () => !inFlight.current && !latest.current.isRunning;
  // The poll's copy of our streamed reply carried memory sources: they go
  // under that reply, the one after the owner message its turn sent (a turn
  // queued behind it may be streaming by now).
  wiring.sink.attachSources = (found, userId) => {
    const reply = replyAfter(latest.current.messages, userId);
    if (reply) sources.add(reply.id, found);
  };

  // History is retried inside storage (getMessages), so this is only the
  // last resort, and it never clears a list that has messages on it. Its
  // loader and error callout are hidden (native.css): the page never shows a
  // web "Loading conversation…" or "Something went wrong".
  useEffect(() => {
    if (!threadError || wiring.historyLoaded.value || isLoadingMessages) return;
    const timer = setTimeout(() => {
      if (latest.current.messages.length === 0) switchToNewThread();
      selectThread(EVE_THREAD.id);
    }, 3000);
    return () => clearTimeout(timer);
  }, [threadError, isLoadingMessages, selectThread, switchToNewThread, wiring]);

  // Native composer -> a turn, exactly as OpenUI's own composer starts one
  // (the store's processMessage). Sends that arrive mid-turn wait their turn,
  // in order, and start once the reply ends (finished, failed or stopped);
  // none is ever folded into the running reply. A send leaves the queue in
  // the same call that hands it to processMessage, which appends it to the
  // thread at once, so its "Queued" bubble and the real one never both show.
  const drain = () => {
    const { isRunning: running, isLoadingMessages: loading, threadError: failed } = latest.current;
    const next = queue.take({
      inFlight: inFlight.current,
      running,
      loading,
      historyLoaded: wiring.historyLoaded.value,
      hiddenAfterError: !!failed && document.visibilityState === "hidden",
    });
    if (next === undefined) return;
    inFlight.current = true;
    const content = ownerParts(next.text, next.attachments);
    void processMessage({ role: "user", content } as Parameters<typeof processMessage>[0]).finally(() => {
      inFlight.current = false;
      setSettled((n) => n + 1);
    });
  };
  const drainRef = useRef(drain);
  drainRef.current = drain;

  useEffect(() => {
    bridge.setSendHandler((text, attachments) => {
      queue.push(text, attachments);
      drainRef.current();
    });
    bridge.setScrollHandler(() => {
      const el = document.querySelector<HTMLElement>(SCROLL_AREA);
      el?.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    });
    const offBusy = bridge.onBusy(setBusy);
    // The native top bar and composer cover the page: pad the list by them
    // (native.css), and keep a list that was at its end pinned there.
    const target = domInsetTarget(SCROLL_AREA);
    const offInsets = bridge.onInsets((insets) => applyInsets(insets, target));
    bridge.post({ type: "ready" });
    return () => {
      bridge.setSendHandler(null);
      bridge.setScrollHandler(null);
      offBusy();
      offInsets();
    };
  }, [bridge, queue]);

  // One continuous thread. Declared after the bridge effect so native hears
  // {type:"ready"} before the first {type:"messages"}.
  useEffect(() => {
    selectThread(EVE_THREAD.id);
  }, [selectThread]);

  // The run settled: the streamed reply is in the store, so lines the poll
  // held during it go in after it (a picture sent mid-turn, before it:
  // insertBeforeReply); then the next queued send starts.
  useEffect(() => {
    if (isRunning || isLoadingMessages || inFlight.current) return;
    wiring.ledger.flushIfIdle();
    drainRef.current();
  }, [isRunning, isLoadingMessages, messages, settled, wiring]);

  // A send held while the page was hidden (sendQueue.ts) goes when it shows.
  useEffect(() => {
    const onVisible = () => {
      if (document.visibilityState === "visible") drainRef.current();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, []);

  // A turn of ours ended: its busy is over, so the dots never wait on a long
  // poll to clear (a poll that still says busy sets them again, and while
  // busy the poll waits 2 s, not 25: storage.ts).
  const poll = useRef<{ stop: () => void; kick: () => void } | null>(null);
  useEffect(() => {
    if (settled === 0) return;
    setBusy(false);
    poll.current?.kick();
  }, [settled]);

  // Live updates while the page is visible.
  useEffect(() => {
    const live = startLivePoll({
      bridge,
      ledger: wiring.ledger,
      cursor: wiring.cursor,
      onBusy: setBusy,
      isVisible: () => document.visibilityState === "visible",
    });
    poll.current = live;
    const onVisible = () => {
      if (document.visibilityState === "visible") live.kick();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      live.stop();
      poll.current = null;
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [bridge, wiring]);

  // Infinite scroll (owner, 2026-10-13: "keep all chat history, i want infinite scroll"): near the top, the page
  // before the first loaded message is fetched and put above, keeping what the owner is reading where it was.
  // Nothing is ever pruned on the Mac, so this goes back to the first day; "more: false" ends it.
  const older = useRef({ loading: false, more: true });
  useEffect(() => {
    const MESSAGE_ID = /^\d{4}-\d{2}-\d{2}:\d+$/;
    const loadOlder = () => {
      const el = document.querySelector<HTMLElement>(SCROLL_AREA);
      const state = older.current;
      if (!el || state.loading || !state.more || !wiring.historyLoaded.value) return;
      if (el.scrollTop > 600 && el.scrollHeight > el.clientHeight) return;
      const current = latest.current.messages;
      const first = current.find((m) => MESSAGE_ID.test(m.id));
      if (!first) return;
      state.loading = true;
      bridge
        .requestOlder(first.id)
        .then((payload) => {
          state.more = payload.more === true;
          pageReactions.ingest(payload);
          const page = mapLines(payload.messages);
          if (page.length === 0) return;
          const now = latest.current.messages;
          const join =
            page.at(-1)?.role === "assistant" && now[0]?.role === "assistant" ? [separator(now[0].id)] : [];
          const height = el.scrollHeight;
          const top = el.scrollTop;
          setMessages([...page, ...join, ...now]);
          requestAnimationFrame(() => {
            el.scrollTop = el.scrollHeight - height + top;   // the same message stays under the owner's eye
          });
        })
        .catch(() => {})                                     // tried again on the next scroll near the top
        .finally(() => {
          state.loading = false;
        });
    };
    const onScroll = (e: Event) => {
      if (e.target instanceof HTMLElement && e.target.matches(SCROLL_AREA)) loadOlder();
    };
    document.addEventListener("scroll", onScroll, { capture: true, passive: true });
    const first = setTimeout(loadOlder, 1500);               // a short first page: fill the screen
    return () => {
      document.removeEventListener("scroll", onScroll, { capture: true });
      clearTimeout(first);
    };
  }, [bridge, setMessages, wiring]);

  // A turn the owner started elsewhere (Telegram, voice) is in flight.
  const showTyping = busy && !isRunning;
  return (
    <>
      <HorizontalHistory bridge={bridge} refreshKey={settled} />
      <QueuedMessages items={queued} />
      {showTyping &&
        createPortal(
          <div className="eve-typing" aria-label="eve is replying">
            <DotMatrixLoader variant="compact" />
          </div>,
          document.body,
        )}
    </>
  );
}

export function App({ bridge }: { bridge: Bridge }) {
  const mode = useSystemThemeMode();

  const { llm, storage, wiring } = useMemo(() => {
    const sink: Wiring["sink"] = {
      append: () => {},
      lastRole: () => undefined,
      idle: () => true,
      insertBeforeReply: () => false,
      attachSources: () => {},
    };
    const ledger = new Ledger(sink);
    const cursor: Cursor = { value: undefined };
    const historyLoaded = { value: false };
    const storage = createEveStorage(bridge, ledger, cursor, () => {
      historyLoaded.value = true;
    });
    return { llm: createBridgeLLM(bridge, ledger), storage, wiring: { ledger, cursor, sink, historyLoaded } };
  }, [bridge]);

  const theme = useMemo(() => ({ mode, lightTheme, darkTheme }), [mode]);
  const components = useMemo(
    () => ({ AssistantMessage: EveAssistantMessage, UserMessage: EveUserMessage }),
    [],
  );

  return (
    <AgentInterface
      llm={llm}
      storage={storage}
      componentLibrary={openuiChatLibrary}
      components={components}
      theme={theme}
      scrollVariant="always"
      artifactAutoOpen={false}
    >
      <AgentInterface.Sidebar>{null}</AgentInterface.Sidebar>
      <AgentInterface.MobileHeader>
        <></>
      </AgentInterface.MobileHeader>
      <AgentInterface.ThreadHeader>
        <></>
      </AgentInterface.ThreadHeader>
      <AgentInterface.Composer>
        <></>
      </AgentInterface.Composer>
      <AgentInterface.Workspace>
        <></>
      </AgentInterface.Workspace>
      <NativeGlue bridge={bridge} wiring={wiring} />
    </AgentInterface>
  );
}
