// The chat: App (the iOS page's own message list, unchanged) under this shell's top bar and above its composer, as
// the iOS app's native chrome sits around its web view (ChatScreen.swift). The shell measures both and tells the
// page how much of it they cover (window.eveNative.setInsets), so the list scrolls under them and its last message
// rests just above the composer.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { App } from "../App";
import type { Bridge } from "../nativeBridge";
import type { Where } from "./api";
import { Composer } from "./Composer";
import type { WebHost } from "./host";
import { MenuIcon, WaveIcon } from "./icons";
import { Lightbox, setViewer, type ShownAttachment } from "./Lightbox";
import { Settings } from "./Settings";
import type { LinkStatus, Transport } from "./transport";
import { trackViewport } from "./viewport";
import type { VoiceController } from "./voice";
import { VoiceView } from "./VoiceView";

/** How far the top bar's fade runs below it; the list's top padding grows by the same (iOS TopFade.overhang). */
const FADE_PX = 20;

const ROUTE_LINE: Record<LinkStatus, string> = {
  loopback: "",
  direct: "",
  connecting: "Connecting…",
  fallback: "Direct link blocked, using HTTPS",
};

const IDEAS = [
  "What can you do?",
  "Remember that I like short answers",
  "Watch the price of gold for me",
  "What's new in tech this week?",
];

function useRoute(transport: Transport): LinkStatus {
  const subscribe = useCallback((cb: () => void) => transport.onStatus(() => cb()), [transport]);
  return useSyncExternalStore(subscribe, () => transport.status);
}

export interface ShellProps {
  where: Where;
  bridge: Bridge;
  host: WebHost;
  transport: Transport;
  voice: VoiceController;
  onAuthLost(): void;
  onSignedOut(): void;
}

export function Shell({ where, bridge, host, transport, voice, onAuthLost, onSignedOut }: ShellProps) {
  const { historyEmpty } = useSyncExternalStore(host.subscribe, host.getState);
  const route = useRoute(transport);
  const [draft, setDraft] = useState("");
  const [panel, setPanel] = useState<"none" | "settings" | "voice">("none");
  const [photo, setPhoto] = useState<ShownAttachment | null>(null);
  const shell = useRef<HTMLDivElement>(null);
  const top = useRef<HTMLElement>(null);
  const bottom = useRef<HTMLDivElement>(null);

  // What the bar and the composer cover, on every change of either (the keyboard, a taller draft, a note).
  useLayoutEffect(() => {
    const measure = () => {
      const s = shell.current?.getBoundingClientRect();
      const t = top.current?.getBoundingClientRect();
      const b = bottom.current?.getBoundingClientRect();
      if (!s || !t || !b || s.height === 0) return;
      host.setInsets({ top: t.bottom - s.top + FADE_PX, bottom: s.bottom - b.top });
    };
    measure();
    const observer = new ResizeObserver(measure);
    for (const el of [shell.current, top.current, bottom.current]) if (el) observer.observe(el);
    return () => observer.disconnect();
  }, [host]);

  useEffect(() => trackViewport(document.documentElement), []);

  // The direct link lives while the chat shows; a page coming back from the background checks it at once.
  useEffect(() => {
    transport.start();
    const wake = () => {
      if (document.visibilityState === "visible") transport.wake();
    };
    document.addEventListener("visibilitychange", wake);
    window.addEventListener("pageshow", wake);
    window.addEventListener("online", wake);
    return () => {
      document.removeEventListener("visibilitychange", wake);
      window.removeEventListener("pageshow", wake);
      window.removeEventListener("online", wake);
      transport.stop();
    };
  }, [transport]);

  // An overlay takes the focus as it opens (autoFocus); as it closes, the focus goes back to what opened it.
  const opener = useRef<HTMLElement | null>(null);
  const remember = () => {
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  };
  const restore = () => {
    const el = opener.current;
    opener.current = null;
    el?.focus();
  };
  const openPanel = (next: "settings" | "voice") => {
    remember();
    setPanel(next);
  };
  useEffect(
    () =>
      setViewer((a) => {
        remember();
        setPhoto(a);
      }),
    [],
  );

  const closePanel = useCallback(() => {
    setPanel("none");
    restore();
  }, []);
  const closePhoto = useCallback(() => {
    setPhoto(null);
    restore();
  }, []);
  const routeLine = ROUTE_LINE[route];
  // The message list renders on its own store's changes only, never on a keystroke in the composer.
  const app = useMemo(() => <App bridge={bridge} />, [bridge]);

  return (
    <div className="eve-shell" ref={shell}>
      <div className="eve-chat">{app}</div>
      {historyEmpty && !draft && (
        <div className="eve-empty">
          <p className="eve-empty__title">eve</p>
          <p className="eve-empty__text">
            Ask anything. eve remembers what you tell it, searches the web, watches prices and pages for you, and uses
            the apps you connect in Settings.
          </p>
          <div className="eve-empty__ideas">
            {IDEAS.map((idea) => (
              <button key={idea} type="button" className="eve-chip" onClick={() => setDraft(idea)}>
                {idea}
              </button>
            ))}
          </div>
        </div>
      )}
      <header className="eve-topbar" ref={top}>
        <button type="button" className="eve-round" aria-label="Settings" onClick={() => openPanel("settings")}>
          <MenuIcon />
        </button>
        <div className="eve-topbar__title">
          <span className="eve-topbar__name">eve</span>
          <span className="eve-topbar__status" role="status">
            {routeLine && (
              <button type="button" className="eve-topbar__route" onClick={() => openPanel("settings")}>
                {routeLine}
              </button>
            )}
          </span>
        </div>
        <button type="button" className="eve-round" aria-label="Talk with eve" onClick={() => openPanel("voice")}>
          <WaveIcon />
        </button>
      </header>
      <div className="eve-composer-wrap" ref={bottom}>
        <Composer host={host} native={bridge.native} draft={draft} setDraft={setDraft} onAuthLost={onAuthLost} />
      </div>
      {panel === "settings" && (
        <Settings where={where} route={route} onClose={closePanel} onAuthLost={onAuthLost} onSignedOut={onSignedOut} />
      )}
      {panel === "voice" && <VoiceView voice={voice} where={where} route={route} onClose={closePanel} />}
      {photo && <Lightbox item={photo} onClose={closePhoto} onAuthLost={onAuthLost} />}
    </div>
  );
}
