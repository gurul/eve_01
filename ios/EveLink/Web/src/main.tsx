import "@openuidev/react-ui/styles/index.css";
import "./native.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { installLinkInterception } from "./links";
import { createBridge, postToNative, uuid, type EveNative } from "./nativeBridge";

declare global {
  interface Window {
    eveNative: EveNative;
  }
}

// OpenUI calls crypto.randomUUID, which only exists in a secure context; a
// custom-scheme page may not be one.
if (typeof crypto !== "undefined" && typeof crypto.randomUUID !== "function") {
  Object.defineProperty(crypto, "randomUUID", { value: uuid, configurable: true });
}

const bridge = createBridge(postToNative);
window.eveNative = bridge.native;
installLinkInterception();

// No pinch or double-tap zoom, whatever the viewport tag allows.
for (const type of ["gesturestart", "gesturechange", "gestureend"]) {
  document.addEventListener(type, (e) => e.preventDefault(), { passive: false });
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App bridge={bridge} />
  </StrictMode>,
);
