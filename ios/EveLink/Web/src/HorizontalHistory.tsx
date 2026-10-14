// Horizontal History: a swipeable row of topic cards that slides up above the
// composer when the owner taps its history button (history.ts). A tap on a
// card closes the row and jumps to where the topic starts in the chat, or,
// when that is no longer loaded, picks the topic back up with eve. The star
// saves it (the daemon keeps bookmarks).

import { useCallback, useEffect, useState } from "react";
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
  const [shown, setShown] = useState(false);
  const [note, setNote] = useState("");

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

  // The composer's history button opens and closes the row; opening asks for fresh topics.
  useEffect(
    () =>
      bridge.onToggleHistory(() =>
        setShown((was) => {
          if (!was) load();
          return !was;
        }),
      ),
    [bridge, load],
  );

  const open = (t: Topic) => {
    bridge.post({ type: "haptic", style: "light" });
    setShown(false);
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
  const now = Date.now() / 1000;
  return createPortal(
    <nav className={`eve-history${shown ? "" : " eve-history--away"}`} aria-label="Conversation history" aria-hidden={!shown}>
      {saved.length === 0 && recent.length === 0 && <p className="eve-history__note">No past topics yet.</p>}
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
