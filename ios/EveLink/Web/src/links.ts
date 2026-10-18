// The web view never navigates away from the chat. Every link goes to
// native ({type:"openURL"}), which opens it in Safari or an in-app browser.

import { postToNative, type Post } from "./nativeBridge";

let post: Post = postToNative;

export function setLinkPost(p: Post): void {
  post = p;
}

// The same schemes as native ChatLinks.external (ios/EveLink/Shared/ChatBridge.swift).
const ALLOWED = /^(https?:|mailto:|tel:|sms:|maps:|facetime:|facetime-audio:)/i;

export function openExternal(url: string): void {
  const trimmed = url.trim();
  if (!ALLOWED.test(trimmed)) return;
  post({ type: "openURL", url: trimmed });
}

export function installLinkInterception(doc: Document = document, win: Window = window): void {
  doc.addEventListener(
    "click",
    (event) => {
      const target = event.target as Element | null;
      const anchor = target?.closest?.("a[href]") as HTMLAnchorElement | null;
      if (!anchor) return;
      const href = anchor.getAttribute("href") ?? "";
      if (href.startsWith("#")) return;
      event.preventDefault();
      event.stopPropagation();
      openExternal(anchor.href || href);
    },
    true,
  );

  // OpenUI components open URLs with window.open; route those too.
  win.open = ((url?: string | URL) => {
    if (url) openExternal(String(url));
    return null;
  }) as typeof win.open;
}

// Bare URLs in a reply become short labelled links, as ChatGPT shows them: "Maps · GUG 220" instead of the whole
// Google Maps address. A URL already inside a markdown link, an <angle> link or code is left alone.
const BARE_URL = /(^|[\s(])((?:https?:\/\/)[^\s<>()\]]+[^\s<>()\].,;:!?'"])/g;

function host(u: URL): string {
  return u.hostname.replace(/^www\./, "");
}

// A path segment that is not valid percent-encoding ("50%off") makes decodeURIComponent throw; show it as written.
function decodeSegment(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

export function linkLabel(raw: string): string {
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return raw;
  }
  const h = host(u);
  const q = u.searchParams.get("query") ?? u.searchParams.get("q") ?? u.searchParams.get("daddr");
  if ((h === "google.com" && u.pathname.startsWith("/maps")) || h === "maps.google.com" || h === "maps.apple.com") {
    return q ? `Maps · ${q.replace(/\+/g, " ").slice(0, 40)}` : "Maps";
  }
  if (h === "youtube.com" || h === "youtu.be") return "YouTube";
  const first = u.pathname.split("/").filter(Boolean)[0];
  return first && first.length <= 24 ? `${h} › ${decodeSegment(first)}` : h;
}

export function compactLinks(markdown: string): string {
  // Leave code, markdown links and <angle> links untouched: split on them, rewrite only the prose between.
  return markdown
    .split(/(```[\s\S]*?```|`[^`\n]*`|\[[^\]\n]*\]\([^)\s]*\)|<https?:[^>\s]+>)/g)
    .map((chunk, i) =>
      i % 2 === 1
        ? chunk
        : chunk.replace(BARE_URL, (_m, lead: string, url: string) => {
            const label = linkLabel(url).replace(/[[\]]/g, "");
            return `${lead}[${label}](${url})`;
          }),
    )
    .join("");
}
