import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import type { CSSOptions, Plugin } from "vite";
import { defineConfig } from "vitest/config";

// The chat page is bundled into the app and served by a WKURLSchemeHandler
// (eveapp://chat/index.html), so every asset path is relative and the
// build is one JS file plus one CSS file, with nothing fetched at runtime.
const outDir = fileURLToPath(new URL("../App/ChatWeb", import.meta.url));

// A custom-scheme page may have an opaque origin, and module scripts and
// `crossorigin` loads are CORS fetches that WebKit can refuse there. Ship a
// classic deferred script instead (the bundle is an IIFE, see output.format).
function classicScript(): Plugin {
  return {
    name: "eve-classic-script",
    apply: "build",
    enforce: "post",
    transformIndexHtml(html) {
      return html
        .replace(/<script type="module" crossorigin/g, "<script defer")
        .replace(/<script type="module"/g, "<script defer")
        .replace(/ crossorigin(="[^"]*")?/g, "");
    },
  };
}

// PostCSS's own types, reached through vite's (postcss is not a direct dependency).
type PostcssPlugins = Extract<NonNullable<CSSOptions["postcss"]>, { plugins?: unknown }>["plugins"];
type PostcssPlugin = Extract<NonNullable<PostcssPlugins>[number], { postcssPlugin: string }>;

// iOS WebKit applies :hover on a tap and keeps it until the next tap
// elsewhere, so a tapped OpenUI button or card stayed highlighted, which is a
// webpage tell. Every selector with :hover moves into @media (hover: hover):
// a pointer still gets it, a finger never does. Selectors in the same rule
// without :hover stay where they were. Inline, so no new dependency.

export function hoverOnlyWithPointer(): PostcssPlugin {
  return {
    postcssPlugin: "eve-hover-only-with-pointer",
    Rule(rule, { AtRule }) {
      const parent = rule.parent;
      const inHoverMedia =
        parent?.type === "atrule" && "name" in parent && parent.name === "media" && /hover:\s*hover/.test(String(parent.params));
      if (inHoverMedia) return;
      const hover = rule.selectors.filter((sel) => sel.includes(":hover"));
      if (hover.length === 0) return;
      const rest = rule.selectors.filter((sel) => !sel.includes(":hover"));
      const media = new AtRule({ name: "media", params: "(hover: hover)" });
      if (rest.length === 0) {
        rule.replaceWith(media);
        media.append(rule);
      } else {
        media.append(rule.clone({ selectors: hover }));
        rule.selectors = rest;
        rule.after(media);
      }
    },
  };
}

export default defineConfig({
  base: "./",
  plugins: [react(), classicScript()],
  css: {
    postcss: { plugins: [hoverOnlyWithPointer()] },
  },
  define: {
    "process.env.NODE_ENV": JSON.stringify("production"),
  },
  build: {
    outDir,
    emptyOutDir: true,
    sourcemap: false,
    cssCodeSplit: false,
    assetsInlineLimit: 100_000_000,
    modulePreload: false,
    target: "safari18",
    // One bundled file by design (the app loads it from disk, not the network).
    chunkSizeWarningLimit: 5000,
    rolldownOptions: {
      output: {
        format: "iife",
        codeSplitting: false,
        entryFileNames: "chat.js",
        assetFileNames: (info) =>
          info.names?.some((n) => n.endsWith(".css")) ? "chat.css" : "[name][extname]",
      },
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
