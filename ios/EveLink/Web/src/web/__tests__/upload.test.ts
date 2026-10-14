import { describe, expect, it, vi } from "vitest";
import {
  HEIC_UNREADABLE,
  MAX_FILE_BYTES,
  NOT_TAKEN,
  PHOTO_PIXELS,
  prepare,
  sniff,
  THUMB_PIXELS,
  upload,
  UploadError,
  type ImageCodec,
  type PendingAttachment,
} from "../upload";

const ascii = (s: string) => Uint8Array.from(s, (c) => c.charCodeAt(0));
const HEIC_HEAD = ascii("\0\0\0\x18ftypheic\0\0\0\0mif1heic");
const JPEG_HEAD = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0, 16, 0x4a, 0x46]);

/** A codec that decodes anything and "encodes" a tagged JPEG of the asked size: the browser's work, stubbed. */
function stubCodec(decodes = true) {
  const jpeg = vi.fn(async (max: number, quality: number) => new Blob([JPEG_HEAD, ascii(`${max}@${quality}`)], { type: "image/jpeg" }));
  const close = vi.fn();
  const codec: ImageCodec = {
    decode: vi.fn(async () => (decodes ? { width: 4032, height: 3024, jpeg, close } : null)),
  };
  return { codec, jpeg, close };
}

describe("attachments, prepared as the iOS app prepares them", () => {
  it("the HEIC path stubbed: a HEIC photo is decoded and goes up as a JPEG of at most 2048 px, with a thumbnail", async () => {
    const { codec, jpeg, close } = stubCodec();
    const file = new File([HEIC_HEAD, new Uint8Array(1000)], "IMG_0042.HEIC", { type: "image/heic" });
    const prepared = await prepare(file, codec);
    expect(prepared.ok).toBe(true);
    if (!prepared.ok) return;
    const item = prepared.item;
    expect(item.name).toBe("IMG_0042.jpg");
    expect(item.isImage).toBe(true);
    expect(item.blob.type).toBe("image/jpeg");
    expect(jpeg.mock.calls).toEqual([
      [PHOTO_PIXELS, 0.82],
      [THUMB_PIXELS, 0.7],
    ]);
    expect(close).toHaveBeenCalledOnce();
    expect(item.thumb).toMatch(/^data:image\/jpeg;base64,[A-Za-z0-9+/]+={0,2}$/);
    // the thumbnail is the small JPEG, as the owner's bubble shows it (attachments.ts takes these)
    expect(atob(item.thumb!.split(",")[1]!)).toContain(`${THUMB_PIXELS}@0.7`);
  });

  it("a HEIC photo is known by its bytes, with no type and no extension", async () => {
    expect(sniff(HEIC_HEAD, "", "photo")).toBe("heif");
    expect(sniff(ascii("\0\0\0\x1cftypavif"), "", "x")).toBe("heif");
    expect(sniff(new Uint8Array(12), "image/heic", "x")).toBe("heif");
    expect(sniff(JPEG_HEAD, "", "x")).toBe("jpeg");
    expect(sniff(ascii("%PDF-1.7"), "", "x")).toBe("pdf");
  });

  it("a HEIC photo this browser cannot decode is refused with a plain line, never sent as HEIC", async () => {
    const { codec } = stubCodec(false);
    const file = new File([HEIC_HEAD], "IMG_1.heic", { type: "" });
    expect(await prepare(file, codec)).toEqual({ ok: false, problem: HEIC_UNREADABLE });
  });

  it("a JPEG the browser could not decode still goes as it is, when it is under the daemon's 10 MB", async () => {
    const { codec } = stubCodec(false);
    const file = new File([JPEG_HEAD, new Uint8Array(100)], "scan.jpg", { type: "image/jpeg" });
    const prepared = await prepare(file, codec);
    expect(prepared).toMatchObject({ ok: true, item: { name: "scan.jpg", isImage: true } });
  });

  it("files: a PDF by its bytes, text and Office by their suffix, anything else refused in the daemon's words", async () => {
    const { codec } = stubCodec();
    const pdf = await prepare(new File([ascii("%PDF-1.4 ...")], "report", { type: "" }), codec);
    expect(pdf).toMatchObject({ ok: true, item: { name: "report", isImage: false } });
    const md = await prepare(new File(["# notes"], "notes.md"), codec);
    expect(md).toMatchObject({ ok: true, item: { name: "notes.md" } });
    const docx = await prepare(new File([ascii("PK\x03\x04")], "plan.docx"), codec);
    expect(docx.ok).toBe(true);
    const zip = await prepare(new File([ascii("PK\x03\x04")], "archive.zip"), codec);
    expect(zip).toEqual({ ok: false, problem: NOT_TAKEN });
    const empty = await prepare(new File([], "empty.txt"), codec);
    expect(empty).toEqual({ ok: false, problem: "That file is empty." });
    expect(codec.decode).not.toHaveBeenCalled();
  });

  it("refuses a file over 20 MB before reading it", async () => {
    const { codec } = stubCodec();
    const big = new File([new Uint8Array(MAX_FILE_BYTES + 1)], "big.pdf");
    expect(await prepare(big, codec)).toEqual({ ok: false, problem: "That file is over 20 MB." });
  });
});

describe("upload", () => {
  const item: PendingAttachment = {
    key: "a1",
    name: "Résumé 2026.pdf",
    isImage: false,
    blob: new Blob([ascii("%PDF-1.4")], { type: "application/pdf" }),
  };

  it("POSTs the raw bytes as octet-stream with the name percent-encoded in X-Filename; checks the daemon's answer", async () => {
    let seen: { url: string; init: RequestInit } | null = null;
    const done = await upload(item, async (url, init = {}) => {
      seen = { url, init };
      return new Response(
        JSON.stringify({ ok: true, id: "0123456789abcdef0123456789abcdef", kind: "pdf", name: "Résumé 2026.pdf", mime: "application/pdf", size: 8 }),
        { status: 200 },
      );
    });
    expect(done).toEqual({ id: "0123456789abcdef0123456789abcdef", kind: "pdf", name: "Résumé 2026.pdf" });
    expect(seen!.url).toBe("/api/web/link/upload");
    expect(seen!.init.method).toBe("POST");
    expect(seen!.init.body).toBe(item.blob);
    const headers = seen!.init.headers as Record<string, string>;
    expect(headers["Content-Type"]).toBe("application/octet-stream");
    expect(headers["X-Filename"]).toBe("R%C3%A9sum%C3%A9%202026.pdf");
    expect(decodeURIComponent(headers["X-Filename"]!)).toBe(item.name);
  });

  it("keeps the thumbnail of a photo for the owner's bubble", async () => {
    const photo: PendingAttachment = { ...item, name: "Photo.jpg", isImage: true, thumb: "data:image/jpeg;base64,AAAA" };
    const done = await upload(photo, async () =>
      new Response(JSON.stringify({ ok: true, id: "ffffffffffffffffffffffffffffffff", kind: "image", name: "Photo.jpg" }), { status: 200 }),
    );
    expect(done.thumb).toBe("data:image/jpeg;base64,AAAA");
  });

  it("says why an upload failed, in the daemon's words when it gave some", async () => {
    const said = (status: number, body: string) => upload(item, async () => new Response(body, { status })).catch((e: unknown) => e);
    const tooBig = await said(413, "");
    expect(tooBig).toBeInstanceOf(UploadError);
    expect((tooBig as UploadError).message).toBe("That file is too large: 20 MB at most.");
    const refused = await said(415, JSON.stringify({ ok: false, error: "Send the photo as JPEG or PNG." }));
    expect((refused as UploadError).message).toBe("Send the photo as JPEG or PNG.");
    const signedOut = await said(401, "");
    expect((signedOut as UploadError).status).toBe(401);
    const odd = await said(200, JSON.stringify({ ok: true, id: "not-an-id", kind: "pdf" }));
    expect((odd as UploadError).message).toBe("eve didn't keep that file.");
    const down = await upload(item, async () => {
      throw new TypeError("network");
    }).catch((e: unknown) => e);
    expect((down as UploadError).status).toBe(0);
  });
});
