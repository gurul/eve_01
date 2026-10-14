// Photos and files from the composer (docs/app-chat.md, "Attachments"), prepared as the iOS app prepares them
// (ios/EveLink/App/ChatAttach.swift): a photo goes as a JPEG of at most 2048 px on its long side, so it stays well
// under the daemon's image limits (10 MB, 25 megapixels), with a small JPEG thumbnail for the owner's bubble. A
// HEIC or AVIF photo is converted on the way when this browser can decode it (the daemon refuses HEIC:
// link_uploads.classify). Anything else goes as it is, up to 20 MB, when the daemon takes its kind. Each is
// POSTed raw to /api/web/link/upload with its name in X-Filename; the id that comes back goes with the turn.

import { pageFetch, UNREACHABLE, type Fetch } from "./api";

export const MAX_ATTACHMENTS = 4;
/** link_uploads.MAX_BYTES */
export const MAX_FILE_BYTES = 20 * 1024 * 1024;
/** telegram_images.MAX_BYTES: an image sent as it is (one this browser could not re-encode). */
const MAX_RAW_IMAGE_BYTES = 10 * 1024 * 1024;
export const PHOTO_PIXELS = 2048;
export const THUMB_PIXELS = 240;
const PHOTO_QUALITY = 0.82;
const THUMB_QUALITY = 0.7;
/** The largest canvas iOS Safari draws (a larger one comes out blank). */
const MAX_CANVAS_AREA = 16_000_000;
const UPLOAD_MS = 120_000;
const ID = /^[0-9a-f]{32}$/;
const KINDS = ["image", "pdf", "text", "doc"] as const;
export type UploadKind = (typeof KINDS)[number];

/** link_uploads' text and Office suffixes; PDFs are known by their bytes. */
const FILE_SUFFIXES = new Set([
  "txt", "md", "markdown", "csv", "tsv", "json", "log", "yaml", "yml", "docx", "xlsx", "pptx", "rtf",
]);
/**
 * The picker's filter. "image/*" and no HEIC type by name: iOS then hands a photo over as a JPEG, while naming
 * HEIC makes it send the original. A HEIC file that arrives anyway (a Mac, Android) is converted in prepare().
 */
export const ACCEPT =
  "image/*,application/pdf,.pdf,.txt,.md,.markdown,.csv,.tsv,.json,.log,.yaml,.yml,.docx,.xlsx,.pptx,.rtf";

export const NOT_TAKEN = "eve takes photos, PDFs, text files and Word, Excel or PowerPoint documents.";
export const HEIC_UNREADABLE = "This browser can't convert that photo. Send it as a JPEG or PNG.";

/** One photo or file picked and not sent yet. */
export interface PendingAttachment {
  key: string;
  name: string;
  isImage: boolean;
  blob: Blob;
  /** data:image/jpeg;base64,…: the preview here, and the owner's bubble on the page. */
  thumb?: string;
}

export type Prepared = { ok: true; item: PendingAttachment } | { ok: false; problem: string };

export interface DecodedImage {
  readonly width: number;
  readonly height: number;
  /** A JPEG of it, scaled to fit `maxPixels` on its long side (never larger than it is). */
  jpeg(maxPixels: number, quality: number): Promise<Blob | null>;
  close(): void;
}

/** How images are decoded and re-encoded: the browser's own, or a stub in tests. */
export interface ImageCodec {
  /** null when this browser cannot decode it. */
  decode(blob: Blob): Promise<DecodedImage | null>;
}

type Sniffed = "jpeg" | "png" | "gif" | "webp" | "heif" | "pdf" | "image" | "other";

const HEIF_BRANDS = new Set(["heic", "heix", "hevc", "hevx", "heim", "heis", "hevm", "hevs", "mif1", "msf1", "avif", "avis"]);
const HEIF_TYPES = /^image\/(heic|heif|avif)(-sequence)?$/i;

function suffix(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot === -1 ? "" : name.slice(dot + 1).toLowerCase();
}

/** What the first bytes (and, failing them, the type and name) say a file is. */
export function sniff(head: Uint8Array, type: string, name: string): Sniffed {
  const ascii = (from: number, to: number) => String.fromCharCode(...head.subarray(from, to));
  if (head[0] === 0xff && head[1] === 0xd8 && head[2] === 0xff) return "jpeg";
  if (ascii(0, 8) === "\x89PNG\r\n\x1a\n") return "png";
  if (ascii(0, 6) === "GIF87a" || ascii(0, 6) === "GIF89a") return "gif";
  if (ascii(0, 4) === "RIFF" && ascii(8, 12) === "WEBP") return "webp";
  if (ascii(4, 8) === "ftyp" && HEIF_BRANDS.has(ascii(8, 12))) return "heif";
  if (ascii(0, 5) === "%PDF-") return "pdf";
  if (HEIF_TYPES.test(type) || ["heic", "heif", "hif", "avif"].includes(suffix(name))) return "heif";
  if (type.startsWith("image/")) return "image";
  return "other";
}

function fit(width: number, height: number, maxPixels: number): { w: number; h: number } {
  let scale = Math.min(1, maxPixels / Math.max(width, height, 1));
  const area = width * height * scale * scale;
  if (area > MAX_CANVAS_AREA) scale *= Math.sqrt(MAX_CANVAS_AREA / area);
  return { w: Math.max(1, Math.round(width * scale)), h: Math.max(1, Math.round(height * scale)) };
}

type Drawable = ImageBitmap | HTMLImageElement;

function decoded(source: Drawable, width: number, height: number, release: () => void): DecodedImage {
  return {
    width,
    height,
    jpeg(maxPixels, quality) {
      const { w, h } = fit(width, height, maxPixels);
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      if (!ctx) return Promise.resolve(null);
      ctx.fillStyle = "#ffffff"; // a transparent PNG becomes white, not black
      ctx.fillRect(0, 0, w, h);
      ctx.imageSmoothingQuality = "high";
      ctx.drawImage(source, 0, 0, w, h);
      return new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/jpeg", quality));
    },
    close: release,
  };
}

/** createImageBitmap, upright; failing that an <img> (which some browsers decode more formats with). */
export const browserCodec: ImageCodec = {
  async decode(blob) {
    if (typeof createImageBitmap === "function") {
      try {
        const bitmap = await createImageBitmap(blob, { imageOrientation: "from-image" });
        return decoded(bitmap, bitmap.width, bitmap.height, () => bitmap.close());
      } catch {
        // not decodable this way; try an <img>
      }
    }
    if (typeof document === "undefined") return null;
    const url = URL.createObjectURL(blob);
    try {
      const img = new Image();
      img.src = url;
      await img.decode();
      if (!img.naturalWidth || !img.naturalHeight) throw new Error("no pixels");
      return decoded(img, img.naturalWidth, img.naturalHeight, () => URL.revokeObjectURL(url));
    } catch {
      URL.revokeObjectURL(url);
      return null;
    }
  },
};

async function dataURL(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return `data:image/jpeg;base64,${btoa(binary)}`;
}

let keys = 0;

function stem(name: string, fallback: string): string {
  const base = name.replace(/\.[^./]*$/, "").trim();
  return base || fallback;
}

/** One picked file → what goes up, or the sentence that says why not. */
export async function prepare(file: File, codec: ImageCodec = browserCodec): Promise<Prepared> {
  if (file.size === 0) return { ok: false, problem: "That file is empty." };
  const head = new Uint8Array(await file.slice(0, 16).arrayBuffer());
  const kind = sniff(head, file.type, file.name);
  const key = `a${++keys}`;
  if (kind === "jpeg" || kind === "png" || kind === "gif" || kind === "webp" || kind === "heif" || kind === "image") {
    const image = await codec.decode(file);
    if (image) {
      try {
        const jpeg = await image.jpeg(PHOTO_PIXELS, PHOTO_QUALITY);
        if (jpeg && jpeg.size > 0) {
          const small = await image.jpeg(THUMB_PIXELS, THUMB_QUALITY);
          const thumb = small && small.size > 0 ? await dataURL(small) : undefined;
          return {
            ok: true,
            item: { key, name: `${stem(file.name, "Photo")}.jpg`, isImage: true, blob: jpeg, ...(thumb ? { thumb } : {}) },
          };
        }
      } finally {
        image.close();
      }
    }
    if (kind === "heif") return { ok: false, problem: HEIC_UNREADABLE };
    if (kind === "image") return { ok: false, problem: NOT_TAKEN };
    if (file.size > MAX_RAW_IMAGE_BYTES) return { ok: false, problem: "That photo is over 10 MB." };
    return { ok: true, item: { key, name: file.name || "Photo", isImage: true, blob: file } };
  }
  if (file.size > MAX_FILE_BYTES) return { ok: false, problem: "That file is over 20 MB." };
  if (kind !== "pdf" && !FILE_SUFFIXES.has(suffix(file.name))) return { ok: false, problem: NOT_TAKEN };
  return { ok: true, item: { key, name: file.name || "File", isImage: false, blob: file } };
}

/** What the daemon kept, as window.eveNative.send takes it (attachments.ts sanitizes it again). */
export interface Uploaded {
  id: string;
  kind: UploadKind;
  name: string;
  thumb?: string;
}

export class UploadError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "UploadError";
  }
}

/** POST one attachment's bytes; its name goes percent-encoded in X-Filename (shown by eve, never a path). */
export async function upload(item: PendingAttachment, fetchFn: Fetch = pageFetch): Promise<Uploaded> {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), UPLOAD_MS);
  let res: Response;
  try {
    res = await fetchFn("/api/web/link/upload", {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      headers: {
        "Content-Type": "application/octet-stream",
        "X-Filename": encodeURIComponent(item.name.slice(0, 200)),
        Accept: "application/json",
      },
      body: item.blob,
      signal: abort.signal,
    });
  } catch {
    throw new UploadError(abort.signal.aborted ? "The upload took too long. Try again." : UNREACHABLE, 0);
  } finally {
    clearTimeout(timer);
  }
  let data: Record<string, unknown> = {};
  try {
    const parsed: unknown = await res.json();
    if (parsed && typeof parsed === "object") data = parsed as Record<string, unknown>;
  } catch {
    // an answer that is not JSON reads as a failure below
  }
  const said = typeof data.error === "string" && data.error.trim() ? data.error.trim().slice(0, 300) : undefined;
  if (!res.ok) {
    if (res.status === 413) throw new UploadError(said ?? "That file is too large: 20 MB at most.", 413);
    throw new UploadError(said ?? `eve answered ${res.status}.`, res.status);
  }
  const { id, kind, name } = data;
  if (data.ok !== true || typeof id !== "string" || !ID.test(id) || !KINDS.includes(kind as UploadKind)) {
    throw new UploadError("eve didn't keep that file.", res.status);
  }
  const shown = typeof name === "string" && name.trim() ? name.slice(0, 120) : kind === "image" ? "Photo" : "File";
  return { id, kind: kind as UploadKind, name: shown, ...(item.thumb && kind === "image" ? { thumb: item.thumb } : {}) };
}
