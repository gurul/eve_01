// eve lite's web app (docs/plans/eve-lite.md, M7): the iOS page's own chat (App.tsx and everything it uses,
// unchanged) run in a browser. createBridge(webPost) stands in for the app's native code: host.ts answers the
// page's bridge messages over the WebRTC link or HTTPS, and the shell (Root, Shell) draws the top bar, the
// composer, voice, settings and the setup and pairing screens around it.

import "@openuidev/react-ui/styles/index.css";
import "../native.css";
import "./web.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { setAttachmentPost } from "../AttachmentList";
import { installLinkInterception, setLinkPost } from "../links";
import { createBridge, type EveNative } from "../nativeBridge";
import { pageFetch, type Where } from "./api";
import { ensureRandomUuid, macHome, whereFrom } from "./boot";
import { spendSecret, type Spent } from "./flow";
import { createWebHost } from "./host";
import { openAttachment } from "./Lightbox";
import { takeFragment } from "./pair";
import { gradePref } from "./prefs";
import { Root } from "./Root";
import { connectPeer } from "./rtc";
import { Transport } from "./transport";
import { browserCanCapture, browserMic, callPortOf, VoiceController, type VoicePeer } from "./voice";
import { keepRealOpen, openScheme, openTab } from "./windows";

declare global {
  interface Window {
    eveNative: EveNative;
  }
}

// A one-time secret in the fragment leaves the URL before anything else runs (pair.ts).
let secret = takeFragment(window.location, window.history);

// The protocol says which door served the page (boot.ts): http is the Mac's own, https the LAN door's, even when
// the Mac itself opens that at https://127.0.0.1 or https://localhost.
const where: Where = whereFrom(window.location.protocol);

// The Mac's cookie is localhost's (`eve-bridge web open` signs in there), so its page at http://127.0.0.1 or
// http://[::1] goes there, unless it brings a secret to spend where it is.
const home = secret ? null : macHome(window.location);
if (home) {
  window.location.replace(home);
} else {
  start();
}

function start(): void {
  // OpenUI calls crypto.randomUUID, which a browser offers only in a secure context: a stand-in where it is missing.
  ensureRandomUuid();

  const authLost = new Set<() => void>();
  const lostAuth = () => {
    for (const listener of [...authLost]) listener();
  };

  keepRealOpen(); // before links.installLinkInterception replaces window.open
  const transport = new Transport({
    loopback: where === "mac",
    connect: (signal) => connectPeer({ signal }),
    fetch: pageFetch,
    onUnauthorized: lostAuth,
    isVisible: () => document.visibilityState !== "hidden",
  });
  const host = createWebHost({
    where,
    transport,
    grade: gradePref.get,
    openWindow: openTab,
    openScheme,
    openAttachment,
    vibrate(pattern) {
      // A vibration before any tap is refused, with a console warning: skip it.
      const nav = navigator as Navigator & { userActivation?: { hasBeenActive: boolean } };
      if (typeof nav.vibrate !== "function" || nav.userActivation?.hasBeenActive === false) return;
      nav.vibrate(pattern);
    },
    onAuthLost: lostAuth,
  });
  const bridge = createBridge(host.post);
  host.attach(bridge.native);
  window.eveNative = bridge.native;
  setLinkPost(host.post);
  setAttachmentPost(host.post);
  installLinkInterception();

  const voice = new VoiceController({
    async peer(): Promise<VoicePeer> {
      const peer = await transport.peer();
      return {
        voice: peer.voice,
        remote: peer.remote,
        setMic: peer.setMic,
        openCall: () => callPortOf(peer.channel("call")),
      };
    },
    release: () => transport.releaseVoice(),
    mic: browserMic,
    canCapture: browserCanCapture,
  });

  let spending: Promise<Spent> | null = null;
  const spent = () => (spending ??= spendSecret(secret));
  const onAuthLost = (listener: () => void) => {
    authLost.add(listener);
    return () => {
      authLost.delete(listener);
    };
  };
  // A secret can also arrive in a tab that is already open (a pairing link pasted into its address bar): the
  // fragment changes without a load. It is taken and cleared the same way, then spent.
  const secretListeners = new Set<() => void>();
  window.addEventListener("hashchange", () => {
    const next = takeFragment(window.location, window.history);
    if (!next) return;
    secret = next;
    spending = null;
    for (const listener of [...secretListeners]) listener();
  });
  const onSecret = (listener: () => void) => {
    secretListeners.add(listener);
    return () => {
      secretListeners.delete(listener);
    };
  };

  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <Root
        where={where}
        spent={spent}
        bridge={bridge}
        host={host}
        transport={transport}
        voice={voice}
        onAuthLost={onAuthLost}
        onSecret={onSecret}
      />
    </StrictMode>,
  );
}
