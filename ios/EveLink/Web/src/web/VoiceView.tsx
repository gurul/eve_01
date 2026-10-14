// Voice: hold the button and talk, let go and eve answers aloud, with captions (docs/voice-mode.md; the call
// itself is voice.ts). A press while eve speaks interrupts it. The first press also starts eve's audio element:
// a browser plays sound only after a tap.

import { useCallback, useEffect, useRef, useSyncExternalStore, type KeyboardEvent, type PointerEvent } from "react";
import type { Where } from "./api";
import { CloseIcon, MicIcon } from "./icons";
import type { LinkStatus } from "./transport";
import type { VoiceController, VoiceFix } from "./voice";

function Fix({ fix, where, early = false }: { fix: VoiceFix; where: Where; early?: boolean }) {
  if (fix === "local-network") {
    return (
      <div className="eve-voice__fix">
        {early && <p>Chat is reaching the Mac over HTTPS right now, so voice may not connect.</p>}
        <p>
          On the Mac, open System Settings, then Privacy &amp; Security, then Local Network, and turn on{" "}
          <b>python3.13</b> (that is eve). Then try again.
        </p>
        {where === "lan" && (
          <p>
            Using Chrome on another Mac? That Mac asks once for Chrome's own Local Network permission: allow it there
            too.
          </p>
        )}
      </div>
    );
  }
  if (fix === "microphone") {
    return (
      <div className="eve-voice__fix">
        <p>In Safari, tap aA in the address bar, then Website Settings, and allow the microphone.</p>
        <p>In Chrome, tap the icon left of the address, then Permissions, and allow the microphone. Then try again.</p>
      </div>
    );
  }
  if (fix === "secure") {
    return (
      <div className="eve-voice__fix">
        <p>Open eve from the link the Mac shows under Settings, Phone access. It starts with https://.</p>
      </div>
    );
  }
  return null;
}

export function VoiceView({
  voice,
  where,
  route,
  onClose,
}: {
  voice: VoiceController;
  where: Where;
  /** How chat reaches the Mac now: "fallback" means the direct link voice needs is blocked at the moment. */
  route: LinkStatus;
  onClose(): void;
}) {
  const state = useSyncExternalStore(voice.subscribe, voice.getState);
  const hold = useRef<HTMLButtonElement>(null);
  const audio = useCallback((el: HTMLAudioElement | null) => voice.attachAudio(el), [voice]);

  useEffect(() => {
    void voice.start();
    // The page hidden or the window left mid-press (a phone call, the lock button, another app): the press ends,
    // as if released, since the release itself will never arrive.
    const onHidden = () => {
      if (document.visibilityState !== "visible") voice.release();
    };
    const onBlur = () => voice.release();
    document.addEventListener("visibilitychange", onHidden);
    window.addEventListener("blur", onBlur);
    return () => {
      document.removeEventListener("visibilitychange", onHidden);
      window.removeEventListener("blur", onBlur);
      voice.stop();
    };
  }, [voice]);

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const down = (e: PointerEvent<HTMLButtonElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture?.(e.pointerId);
    voice.press();
  };
  const up = () => voice.release();
  const keyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if ((e.key === " " || e.key === "Enter") && !e.repeat) {
      e.preventDefault();
      voice.press();
    }
  };
  const keyUp = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key === " " || e.key === "Enter") {
      e.preventDefault();
      voice.release();
    }
  };

  const live = ["ready", "talking", "thinking", "speaking"].includes(state.phase);
  const retry = state.phase === "unavailable" || state.phase === "ended";

  // The button takes the keyboard once it works (Space or Enter held is a press, as a finger is).
  useEffect(() => {
    if (live) hold.current?.focus();
  }, [live]);

  return (
    <div className="eve-voice" role="dialog" aria-modal="true" aria-label="Talk with eve">
      <audio ref={audio} playsInline autoPlay />
      <div className="eve-voice__top">
        <button type="button" className="eve-round eve-round--dark" aria-label="End voice" onClick={onClose}>
          <CloseIcon />
        </button>
      </div>
      <div className="eve-voice__captions" aria-live="polite">
        {state.heard ? <p className="eve-voice__heard">{state.heard}</p> : null}
        {state.said ? <p className="eve-voice__said">{state.said}</p> : null}
      </div>
      <div className="eve-voice__bottom">
        <p className="eve-voice__status" role="status">
          {state.status}
        </p>
        {state.fix ? (
          <Fix fix={state.fix} where={where} />
        ) : (
          state.phase === "connecting" && route === "fallback" && <Fix fix="local-network" where={where} early />
        )}
        {retry ? (
          <button type="button" className="eve-button eve-button--light" onClick={() => void voice.start()}>
            {state.phase === "ended" ? "Call again" : "Try again"}
          </button>
        ) : (
          <button
            ref={hold}
            type="button"
            className={state.phase === "talking" ? "eve-hold eve-hold--down" : "eve-hold"}
            aria-label="Hold to talk"
            aria-pressed={state.phase === "talking"}
            disabled={!live}
            onPointerDown={down}
            onPointerUp={up}
            onPointerCancel={up}
            onLostPointerCapture={up}
            onKeyDown={keyDown}
            onKeyUp={keyUp}
            onBlur={up}
            onContextMenu={(e) => e.preventDefault()}
          >
            <MicIcon />
          </button>
        )}
      </div>
    </div>
  );
}
