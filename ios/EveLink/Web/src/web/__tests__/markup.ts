// Screens drawn to HTML for the tests (react-dom/server: these tests have no DOM), read back as a person sees them.
// Not a test file itself: vitest collects *.test.ts only.

import { createElement, type FunctionComponent } from "react";
import { renderToStaticMarkup } from "react-dom/server";

export function draw<P extends object>(component: FunctionComponent<P>, props: P): string {
  return renderToStaticMarkup(createElement(component, props));
}

const ENTITIES: Record<string, string> = { "&amp;": "&", "&lt;": "<", "&gt;": ">", "&quot;": '"', "&#x27;": "'" };

/** The words on the screen: tags out, entities read, white space folded. */
export function text(html: string): string {
  return html
    .replace(/<[^>]*>/g, " ")
    .replace(/&(?:amp|lt|gt|quot|#x27);/g, (entity) => ENTITIES[entity] ?? entity)
    .replace(/\s+/g, " ")
    .trim();
}

/** The opening tag of the link whose words are `label`, or "" when there is none. */
export function link(html: string, label: string): string {
  return new RegExp(`(<a [^>]*>)${label}</a>`).exec(html)?.[1] ?? "";
}

/** The words of the nth list item, from 1 (a setup step). */
export function item(html: string, n: number): string {
  return text(html.match(/<li[\s>][\s\S]*?<\/li>/g)?.[n - 1] ?? "");
}
