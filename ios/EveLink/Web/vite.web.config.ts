import react from "@vitejs/plugin-react";
import { existsSync, renameSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import { hoverOnlyWithPointer } from "./vite.config.ts";

// eve lite's web app (docs/plans/eve-lite.md, M7): the second entry, web.html and src/web/, built into the
// bridge's package (bridge/src/eve_bridge/web_lite/, generated and git-ignored), where lite_web.py serves it at
// `/` and `/assets/*` on both of the Mac's doors. The iOS chat's build (vite.config.ts, ../App/ChatWeb) is
// separate and unchanged.
const root = fileURLToPath(new URL(".", import.meta.url));
const outDir = fileURLToPath(new URL("../../../bridge/src/eve_bridge/web_lite", import.meta.url));

// The entry is web.html (index.html is the iOS page's); the door serves index.html.
function servedAsIndex(): Plugin {
  return {
    name: "eve-web-index",
    apply: "build",
    writeBundle() {
      const built = join(outDir, "web.html");
      if (existsSync(built)) renameSync(built, join(outDir, "index.html"));
    },
  };
}

export default defineConfig({
  root,
  base: "/",
  plugins: [react(), servedAsIndex()],
  css: {
    // iOS Safari keeps :hover stuck after a tap, in a browser as in the app's web view (vite.config.ts).
    postcss: { plugins: [hoverOnlyWithPointer()] },
  },
  define: {
    "process.env.NODE_ENV": JSON.stringify("production"),
  },
  build: {
    outDir,
    emptyOutDir: true,
    sourcemap: false,
    // Phones from about 2023 on: iOS 16.4 Safari, Chrome 111 (color-mix, RTCRtpSender.replaceTrack, modules).
    target: ["safari16.4", "chrome111", "firefox114", "edge111"],
    cssCodeSplit: false,
    modulePreload: false,
    // One script and one stylesheet, as the iOS chat ships: the Mac's small server answers two requests, not one
    // per icon OpenUI could load (code splitting made about 1,600 files). A few MB over the home Wi-Fi is fine.
    chunkSizeWarningLimit: 5000,
    rolldownOptions: {
      input: fileURLToPath(new URL("web.html", import.meta.url)),
      output: {
        codeSplitting: false,
        entryFileNames: "assets/eve-[hash].js",
        assetFileNames: "assets/eve-[hash][extname]",
      },
    },
  },
});
