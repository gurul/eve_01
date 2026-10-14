// A tapped attachment (the owner's or eve's), opened as the iOS app opens one in Quick Look: a photo full size in
// the page, with Save; a PDF in a new tab, where the browser shows it; any other file downloads. The page fetches
// the photo itself with its own cookie (GET /api/web/link/file/<id>), so this works in a home-screen web app too,
// whose cookies are its own.

import { useEffect, useState } from "react";
import { pageFetch, UNREACHABLE } from "./api";
import { CloseIcon } from "./icons";
import { openTab, saveBlob } from "./windows";

export interface ShownAttachment {
  id: string;
  name: string;
  kind: string;
}

type Viewer = (a: ShownAttachment) => void;
let viewer: Viewer | null = null;

/** The shell shows photos; while it is not mounted a photo opens like any other file. */
export function setViewer(v: Viewer): () => void {
  viewer = v;
  return () => {
    if (viewer === v) viewer = null;
  };
}

export const fileURL = (id: string) => `/api/web/link/file/${id}`;

/** host.openAttachment: a photo in the viewer, a PDF or anything else in a new tab. Called inside the tap. */
export function openAttachment(a: ShownAttachment): void {
  if (a.kind === "image" && viewer) viewer(a);
  else openTab(fileURL(a.id));
}

const GONE = "That file is gone. eve keeps pictures and files for a day.";

export function Lightbox({ item, onClose, onAuthLost }: { item: ShownAttachment; onClose(): void; onAuthLost(): void }) {
  const [shown, setShown] = useState<{ url: string; blob: Blob } | null>(null);
  const [problem, setProblem] = useState("");

  useEffect(() => {
    const abort = new AbortController();
    let url = "";
    void (async () => {
      try {
        const res = await pageFetch(fileURL(item.id), { credentials: "same-origin", cache: "no-store", signal: abort.signal });
        if (res.status === 401) {
          onAuthLost();
          return;
        }
        if (!res.ok) {
          setProblem(res.status === 404 ? GONE : `eve answered ${res.status}.`);
          return;
        }
        const blob = await res.blob();
        if (abort.signal.aborted) return;
        url = URL.createObjectURL(blob);
        setShown({ url, blob });
      } catch {
        if (!abort.signal.aborted) setProblem(UNREACHABLE);
      }
    })();
    return () => {
      abort.abort();
      if (url) URL.revokeObjectURL(url);
    };
  }, [item.id, onAuthLost]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="eve-lightbox" role="dialog" aria-modal="true" aria-label={item.name} onClick={onClose}>
      <div className="eve-lightbox__bar" onClick={(e) => e.stopPropagation()}>
        <span className="eve-lightbox__name">{item.name}</span>
        {shown && (
          <button type="button" className="eve-button eve-button--light" onClick={() => saveBlob(shown.blob, item.name)}>
            Save
          </button>
        )}
        <button type="button" className="eve-round eve-round--dark" aria-label="Close" onClick={onClose} autoFocus>
          <CloseIcon />
        </button>
      </div>
      {shown ? (
        <img className="eve-lightbox__img" src={shown.url} alt={item.name} onClick={(e) => e.stopPropagation()} />
      ) : (
        <p className="eve-lightbox__note" role="status">
          {problem || "Opening…"}
        </p>
      )}
    </div>
  );
}
