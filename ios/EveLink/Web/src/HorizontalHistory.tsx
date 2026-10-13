// Horizontal History: a swipeable row of topic cards over the top of the chat
// (history.ts). It shows when the page opens and when the owner scrolls up,
// and slides away while they read down. A tap jumps to where the topic starts
// in the chat, or, when that is no longer loaded, picks the topic back up with
// eve. The star saves it (the daemon keeps bookmarks).

import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { cards, parseTopics, pickUp, startOf, when, withBookmark, type Topic, type TopicsView } from "./history";
import type { Bridge } from "./nativeBridge";

const SCROLL_AREA = ".openui-agent-thread-scroll-area";
const HIGHLIGHT_MS = 1600;

function loadedIds(): string[] {
  return Array.from(document.querySelectorAll<HTMLElement>("[data-mid]")).map((el) => el.dataset.mid ?? "");
}

function jumpTo(id: string): void {
  const marker = document.querySelector<HTMLElement>(`[data-mid="${CSS.escape(id)}"]`);
  const el = marker && marker.getBoundingClientRect().height === 0 ? (marker.firstElementChild as HTMLElement | null) : marker;
  if (!el) return;
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  el.scrollIntoView({ behavior: reduce ? "auto" : "smooth", block: "center" });
  el.classList.add("eve-topic-flash");
  setTimeout(() => el.classList.remove("eve-topic-flash"), HIGHLIGHT_MS);
}

function Card({ t, now, onOpen, onStar }: { t: Topic; now: number; onOpen: () => void; onStar: () => void }) {
  const meta = `${t.count} ${t.count === 1 ? "message" : "messages"} · ${when(t.ended_at, now)}`;
  return (
    <div className="eve-topic">
      <button type="button" className="eve-topic__open" onClick={onOpen} aria-label={`${t.title}, ${meta}`}>
        <span className="eve-topic__title">{t.title}</span>
        {t.preview && <span className="eve-topic__preview">{t.preview}</span>}
        <span className="eve-topic__meta">{meta}</span>
      </button>
      <button
        type="button"
        className="eve-topic__star"
        aria-pressed={t.bookmarked}
        aria-label={t.bookmarked ? "Remove from saved" : "Save topic"}
        onClick={onStar}
      >
        <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
          <path d="M12 3.6l2.5 5.2 5.7.8-4.1 4 1 5.7L12 16.6l-5.1 2.7 1-5.7-4.1-4 5.7-.8z" />
        </svg>
      </button>
    </div>
  );
}

export function HorizontalHistory({ bridge, refreshKey }: { bridge: Bridge; refreshKey: number }) {
  const [view, setView] = useState<TopicsView>({ topics: [], bookmarked: [] });
  const [shown, setShown] = useState(true);
  const [note, setNote] = useState("");
  const nav = useRef<HTMLElement | null>(null);

  const load = useCallback(() => {
    bridge
      .requestLink({ type: "topics" })
      .then((raw) => setView(parseTopics(raw)))
      .catch(() => {}); // no cards is a quiet state; the next turn or visit asks again
  }, [bridge]);

  // On open, after each settled turn, and when the page comes back.
  useEffect(() => {
    load();
  }, [load, refreshKey]);
  useEffect(() => {
    const onVisible = () => document.visibilityState === "visible" && load();
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [load]);

  // Scrolling up shows the row, scrolling down hides it. The list mounts
  // after this component, so it is looked up on each event.
  useEffect(() => {
    let last = -1;
    const onScroll = (e: Event) => {
      const el = e.target as HTMLElement;
      if (!(el instanceof HTMLElement) || !el.matches(SCROLL_AREA)) return;
      const y = el.scrollTop;
      if (last >= 0 && Math.abs(y - last) > 8) setShown(y < last);
      last = y;
    };
    document.addEventListener("scroll", onScroll, { capture: true, passive: true });
    return () => document.removeEventListener("scroll", onScroll, { capture: true });
  }, []);

  // The row covers the top of the list: the list makes room for it above the first message (native.css,
  // --eve-history-h). Kept while the row is away, so hiding it never moves what the owner is reading.
  const hasCards = view.topics.length + view.bookmarked.length > 0;
  useEffect(() => {
    const root = document.documentElement.style;
    const el = nav.current;
    if (!hasCards || !el) {
      root.setProperty("--eve-history-h", "0px");
      return;
    }
    const set = () => root.setProperty("--eve-history-h", `${Math.ceil(el.getBoundingClientRect().height)}px`);
    set();
    const ro = new ResizeObserver(set);
    ro.observe(el);
    return () => ro.disconnect();
  }, [hasCards]);

  const open = (t: Topic) => {
    bridge.post({ type: "haptic", style: "light" });
    const start = startOf(t, loadedIds());
    if (start) jumpTo(start);
    else bridge.native.send(pickUp(t));
  };

  const star = (t: Topic) => {
    const on = !t.bookmarked;
    bridge.post({ type: "haptic", style: "light" });
    setView((v) => withBookmark(v, t.id, on));
    setNote("");
    bridge.requestLink({ type: "bookmark", topic: t.id, on }).catch(() => {
      setView((v) => withBookmark(v, t.id, !on));
      setNote(on ? "Couldn't save that topic. Try again." : "Couldn't remove that topic. Try again.");
    });
  };

  const { saved, recent } = cards(view);
  if (saved.length === 0 && recent.length === 0) return null;
  const now = Date.now() / 1000;
  return createPortal(
    <nav ref={nav} className={`eve-history${shown ? "" : " eve-history--away"}`} aria-label="Conversation history">
      <div className="eve-history__row">
        {saved.map((t) => (
          <Card key={`s-${t.id}`} t={t} now={now} onOpen={() => open(t)} onStar={() => star(t)} />
        ))}
        {recent.map((t) => (
          <Card key={`r-${t.id}`} t={t} now={now} onOpen={() => open(t)} onStar={() => star(t)} />
        ))}
      </div>
      {note && (
        <p className="eve-history__note" role="status">
          {note}
        </p>
      )}
    </nav>,
    document.body,
  );
}
