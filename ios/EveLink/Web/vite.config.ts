import react from "@vitejs/plugin-react";
import type { CSSOptions } from "vite";
import { defineConfig } from "vitest/config";

// PostCSS's own types, reached through vite's (postcss is not a direct dependency).
type PostcssPlugins = Extract<NonNullable<CSSOptions["postcss"]>, { plugins?: unknown }>["plugins"];
type PostcssPlugin = Extract<NonNullable<PostcssPlugins>[number], { postcssPlugin: string }>;

// WebKit on a phone applies :hover on a tap and keeps it until the next tap
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

// The tests' configuration, and the PostCSS plugin eve lite's build shares (vite.web.config.ts). The iPhone app no
// longer bundles a page: its chat is native (ios/EveLink/Packages), so there is no build here.
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
