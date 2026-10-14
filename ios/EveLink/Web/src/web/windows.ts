// Opening things outside the chat. links.installLinkInterception replaces window.open (so every link in a reply
// goes through openURL) and catches every link click in the page, so the real window.open is kept here first
// (main.tsx), and downloads and system links go through links that are never in the page.

type Open = (url?: string | URL, target?: string, features?: string) => Window | null;

let realOpen: Open | null = null;

/** Call before links.installLinkInterception. */
export function keepRealOpen(win: Window = window): void {
  if (!realOpen) realOpen = win.open.bind(win);
}

/** A web page in a new tab, with no opener and no referrer. */
export function openTab(url: string): void {
  realOpen?.(url, "_blank", "noopener,noreferrer");
}

/** A link clicked outside the page's tree, so the page's link interception never sees it. */
function detachedClick(href: string, setup: (a: HTMLAnchorElement) => void = () => {}): void {
  const a = document.createElement("a");
  a.href = href;
  a.rel = "noopener noreferrer";
  setup(a);
  a.click();
}

/** mailto:, tel: and sms: go to the system's own handler (Mail, a call, Messages); the page stays. */
export function openScheme(url: string): void {
  detachedClick(url);
}

const EXTENSIONS: Record<string, string> = {
  "image/jpeg": "jpg",
  "image/png": "png",
  "image/gif": "gif",
  "image/webp": "webp",
  "application/pdf": "pdf",
};

/** The name to save under: an extension from the type when the name has none ("Photo" → "Photo.jpg"). */
export function saveName(name: string, type: string): string {
  const clean = name.replace(/[\\/:*?"<>|\u0000-\u001f]/g, " ").trim() || "eve file";
  const ext = EXTENSIONS[type.split(";")[0]?.trim().toLowerCase() ?? ""];
  return ext && !/\.[A-Za-z0-9]{1,5}$/.test(clean) ? `${clean}.${ext}` : clean;
}

/** Save a file this page fetched, under its name. */
export function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  detachedClick(url, (a) => {
    a.download = saveName(name, blob.type);
  });
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/**
 * A tab opened now, inside the tap (where no pop-up blocker stops it), and sent to a URL that is still being
 * fetched (a Composio Connect Link). It is cut off from this page first, so the page it shows can never reach
 * back. `cancel` closes it when there is nowhere to send it.
 */
export function openPending(): { go(url: string): void; cancel(): void } {
  let tab: Window | null = null;
  try {
    tab = realOpen?.("about:blank", "_blank") ?? null;
  } catch {
    tab = null;
  }
  if (tab) {
    try {
      tab.opener = null;
    } catch {
      // some browsers already give a new tab no opener
    }
  }
  return {
    go(url) {
      if (tab && !tab.closed) {
        try {
          tab.location.replace(url);
          return;
        } catch {
          // fall through to a fresh tab
        }
      }
      openTab(url);
    },
    cancel() {
      try {
        tab?.close();
      } catch {
        // already closed
      }
    },
  };
}

/** An iPhone or iPad (an iPad's Safari says it is a Mac, but a Mac has no touch screen). */
export function appleMobile(nav: Navigator = navigator): boolean {
  return /iP(hone|ad|od)/.test(nav.userAgent) || (/Macintosh/.test(nav.userAgent) && nav.maxTouchPoints > 1);
}
