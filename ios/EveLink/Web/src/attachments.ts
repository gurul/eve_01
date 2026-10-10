// Photos and files the owner attached in the native composer (docs/app-chat.md,
// "Attachments"). Native uploads them, then calls
// window.eveNative.send(text, json) with what came back; the page keeps
// them in the owner's message as AG-UI "binary" parts, shows them above the
// bubble, and sends their ids with the turn. History brings them back from
// /api/link/messages ("attachments" on an owner line).
//
// eve's own pictures and files (a photo it took, a screenshot, a file it
// sent) come back the same way, on an eve line ("media"). An assistant
// message's content is a string, so their attachments are kept beside it,
// by message id (eveMedia), and AssistantMessage draws them. A tap on any
// attachment asks native to open it full size (AttachmentList.tsx).

/** "file": eve's own only, anything the upload door would not take (a zip). */
export type AttachmentKind = "image" | "pdf" | "text" | "doc" | "file";

export interface Attachment {
  id: string;
  name: string;
  kind: AttachmentKind;
  /** A small JPEG as a data: URL (images only), while the upload lives. */
  thumb?: string;
  /** The upload expired (kept 24 h): shown by name, nothing to open. */
  gone?: boolean;
}

export const MAX_ATTACHMENTS = 4;
const ID = /^[0-9a-f]{32}$/;
const KINDS: readonly AttachmentKind[] = ["image", "pdf", "text", "doc", "file"];
// A data: URL of a JPEG or PNG and nothing else: never a URL the page would fetch.
const THUMB = /^data:image\/(jpeg|png);base64,[A-Za-z0-9+/]+={0,2}$/;
const MAX_THUMB_CHARS = 400_000;

const MIME: Record<AttachmentKind, string> = {
  image: "image/jpeg",
  pdf: "application/pdf",
  text: "text/plain",
  doc: "application/octet-stream",
  // never sent to the daemon (an owner part is an upload, never "file"); distinct, so kindOfMime round-trips
  file: "application/x-eve-file",
};

function kindOfMime(mime: unknown): AttachmentKind | null {
  for (const k of KINDS) if (MIME[k] === mime) return k;
  return null;
}

/** Well-formed attachments only (native's JSON, or a history line's), at most MAX_ATTACHMENTS. */
export function sanitizeAttachments(raw: unknown): Attachment[] {
  if (!Array.isArray(raw)) return [];
  const out: Attachment[] = [];
  for (const a of raw) {
    if (out.length >= MAX_ATTACHMENTS) break;
    if (!a || typeof a !== "object") continue;
    const { id, name, kind, thumb, gone } = a as Record<string, unknown>;
    if (typeof id !== "string" || !ID.test(id)) continue;
    if (typeof kind !== "string" || !KINDS.includes(kind as AttachmentKind)) continue;
    const att: Attachment = {
      id,
      name: typeof name === "string" && name.trim() ? name.slice(0, 120) : kind === "image" ? "Photo" : "File",
      kind: kind as AttachmentKind,
    };
    if (gone === true) {
      att.gone = true;
    } else if (kind === "image" && typeof thumb === "string" && thumb.length <= MAX_THUMB_CHARS && THUMB.test(thumb)) {
      att.thumb = thumb;
    }
    out.push(att);
  }
  return out;
}

// ---- eve's own: kept beside the assistant message ----------------------------------------------------------

const eveMedia = new Map<string, Attachment[]>();

/** The attachments of eve's message `id` (a "media" line), set when the line is mapped. */
export function setEveMedia(id: string, attachments: Attachment[]): void {
  if (attachments.length) eveMedia.set(id, attachments);
  else eveMedia.delete(id);
}

export function eveMediaOf(id: string): Attachment[] {
  return eveMedia.get(id) ?? [];
}

/** native's send(text, json): the attachments it carries, or none for anything malformed. */
export function parseNativeAttachments(json: unknown): Attachment[] {
  if (typeof json !== "string" || !json) return [];
  try {
    return sanitizeAttachments(JSON.parse(json));
  } catch {
    return [];
  }
}

export interface BinaryPart {
  type: "binary";
  mimeType: string;
  id: string;
  filename: string;
  url?: string;
}

export type OwnerContent = string | ({ type: "text"; text: string } | BinaryPart)[];

/** An owner message's content: the text alone, or the text and one binary part per attachment. */
export function ownerParts(text: string, attachments: Attachment[]): OwnerContent {
  if (attachments.length === 0) return text;
  return [
    { type: "text", text },
    ...attachments.map(
      (a): BinaryPart => ({
        type: "binary",
        mimeType: MIME[a.kind],
        id: a.id,
        filename: a.name,
        ...(a.thumb ? { url: a.thumb } : {}),
      }),
    ),
  ];
}

/** The attachments in a message's content (its binary parts). */
export function attachmentsOf(content: unknown): Attachment[] {
  if (!Array.isArray(content)) return [];
  return sanitizeAttachments(
    content
      .filter((p) => p && typeof p === "object" && (p as { type?: unknown }).type === "binary")
      .map((p) => {
        const b = p as Partial<BinaryPart>;
        return { id: b.id, name: b.filename, kind: kindOfMime(b.mimeType), thumb: b.url };
      }),
  );
}

/** The ids a turn sends to the daemon. */
export function attachmentIds(content: unknown): string[] {
  return attachmentsOf(content).map((a) => a.id);
}

/** The text of a message's content, without its attachments. */
export function textOf(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((p) => p && typeof p === "object" && (p as { type?: unknown }).type === "text")
    .map((p) => String((p as { text: unknown }).text))
    .join("\n");
}
