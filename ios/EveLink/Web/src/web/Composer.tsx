// The composer, outside App so App.tsx stays the iOS page's own (docs/plans/eve-lite.md, M7): one rounded card
// with the picked photos and files, the field, and under them attach, the Think toggle and send. While eve answers,
// send stays send whenever there is something to send (the page queues it after the reply: sendQueue.ts) and is
// stop otherwise. As in the iOS app (ChatScreen.swift, ChatComposer), the attachments are uploaded first and the
// words go to the page with window.eveNative.send, so a turn starts exactly as one typed in OpenUI.

import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type ChangeEvent, type ClipboardEvent, type KeyboardEvent } from "react";
import type { EveNative } from "../nativeBridge";
import { MAX_TEXT, scalars, type WebHost } from "./host";
import { ArrowUpIcon, CloseIcon, FileIcon, PlusIcon, SparkIcon, StopIcon } from "./icons";
import { gradePref } from "./prefs";
import { ACCEPT, MAX_ATTACHMENTS, prepare, upload, UploadError, type PendingAttachment, type Uploaded } from "./upload";

const MAX_FIELD_PX = 168; // about six lines

export interface ComposerProps {
  host: WebHost;
  native: EveNative;
  draft: string;
  setDraft: (next: string | ((prev: string) => string)) => void;
  onAuthLost(): void;
}

const coarse = () => typeof matchMedia === "function" && matchMedia("(pointer: coarse)").matches;

export function Composer({ host, native, draft, setDraft, onAuthLost }: ComposerProps) {
  const { ready, busy, problem } = useSyncExternalStore(host.subscribe, host.getState);
  const grade = useSyncExternalStore(gradePref.subscribe, gradePref.get);
  const [attachments, setAttachments] = useState<PendingAttachment[]>([]);
  const [preparing, setPreparing] = useState(0);
  const [uploading, setUploading] = useState(false);
  const [attachProblem, setAttachProblem] = useState("");
  const field = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);

  const text = draft.trim();
  const tooLong = scalars(text) > MAX_TEXT;
  const canSend = ready && !uploading && preparing === 0 && (text.length > 0 || attachments.length > 0) && !tooLong;
  const roomLeft = MAX_ATTACHMENTS - attachments.length;

  // The field grows with its text, up to about six lines, then scrolls.
  useLayoutEffect(() => {
    const el = field.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, MAX_FIELD_PX)}px`;
  }, [draft]);

  useEffect(() => {
    if (problem) setAttachProblem("");
  }, [problem]);

  const addFiles = async (files: File[]) => {
    setAttachProblem("");
    host.clearProblem();
    if (files.length > roomLeft) setAttachProblem("Four attachments at most.");
    const take = files.slice(0, Math.max(roomLeft, 0));
    setPreparing((n) => n + take.length);
    for (const file of take) {
      try {
        const prepared = await prepare(file);
        if (prepared.ok) {
          setAttachments((list) => (list.length < MAX_ATTACHMENTS ? [...list, prepared.item] : list));
        } else {
          setAttachProblem(prepared.problem);
        }
      } catch {
        setAttachProblem("Couldn't read that file.");
      } finally {
        setPreparing((n) => n - 1);
      }
    }
  };

  const onPick = (e: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? []);
    e.target.value = ""; // the same file can be picked again
    if (files.length) void addFiles(files);
  };

  const onPaste = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(e.clipboardData?.files ?? []);
    if (files.length === 0) return;
    e.preventDefault();
    void addFiles(files);
  };

  const send = async () => {
    if (!canSend) return;
    setAttachProblem("");
    host.clearProblem();
    if (attachments.length === 0) {
      native.send(text);
      setDraft("");
      native.scrollToBottom();
      return;
    }
    // Upload first, then the turn names them. A failed upload keeps everything here, with one line saying why.
    const items = attachments;
    setUploading(true);
    const done: Uploaded[] = [];
    for (const item of items) {
      try {
        done.push(await upload(item));
      } catch (e) {
        setUploading(false);
        if (e instanceof UploadError && e.status === 401) onAuthLost();
        setAttachProblem(e instanceof Error ? e.message : "That file didn't upload.");
        return;
      }
    }
    native.send(text, JSON.stringify(done));
    setDraft((now) => (now.trim() === text ? "" : now));
    setAttachments((list) => list.filter((a) => !items.includes(a)));
    setUploading(false);
    native.scrollToBottom();
  };

  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key !== "Enter" || e.shiftKey || e.altKey || e.nativeEvent.isComposing || coarse()) return;
    e.preventDefault();
    void send();
  };

  // One line above the card, the most pressing first (as the iOS composer's note).
  const note = tooLong ? `Too long: ${MAX_TEXT} characters at most.` : attachProblem || problem;

  return (
    <div className="eve-composer">
      {note && (
        <p className="eve-composer__note" role="alert">
          {note}
        </p>
      )}
      <div className="eve-composer__card" onClick={() => ready && field.current?.focus()}>
        {(attachments.length > 0 || preparing > 0) && (
          <ul className="eve-composer__strip" aria-label="Attachments">
            {attachments.map((a) => (
              <li key={a.key} className="eve-composer__item">
                {a.thumb ? (
                  <img className="eve-composer__thumb" src={a.thumb} alt={`Photo ${a.name}`} draggable={false} />
                ) : (
                  <span className="eve-composer__file">
                    <FileIcon />
                    <span className="eve-composer__filename">{a.name}</span>
                  </span>
                )}
                <button
                  type="button"
                  className="eve-composer__remove"
                  aria-label={`Remove ${a.name}`}
                  disabled={uploading}
                  onClick={(e) => {
                    e.stopPropagation();
                    setAttachments((list) => list.filter((x) => x !== a));
                  }}
                >
                  <CloseIcon />
                </button>
              </li>
            ))}
            {preparing > 0 && <li className="eve-composer__item eve-composer__item--busy" aria-label="Preparing" />}
          </ul>
        )}
        <textarea
          ref={field}
          className="eve-composer__field"
          rows={1}
          value={draft}
          placeholder="Ask eve"
          aria-label="Message eve"
          disabled={!ready}
          enterKeyHint={coarse() ? "enter" : "send"}
          onChange={(e) => {
            setDraft(e.target.value);
            if (problem) host.clearProblem();
          }}
          onKeyDown={onKey}
          onPaste={onPaste}
        />
        <div className="eve-composer__row">
          <button
            type="button"
            className="eve-round eve-round--plain"
            aria-label="Add a photo or file"
            title={roomLeft <= 0 ? "Four attachments at most" : "Add a photo or file"}
            disabled={!ready || roomLeft <= 0 || uploading}
            onClick={(e) => {
              e.stopPropagation();
              picker.current?.click();
            }}
          >
            <PlusIcon />
          </button>
          <input ref={picker} type="file" accept={ACCEPT} multiple hidden onChange={onPick} />
          <span className="eve-composer__spacer" />
          <button
            type="button"
            className={grade === "think" ? "eve-chip eve-chip--on" : "eve-chip"}
            aria-pressed={grade === "think"}
            title="Think longer for better answers"
            onClick={(e) => {
              e.stopPropagation();
              gradePref.set(grade === "think" ? "auto" : "think");
            }}
          >
            <SparkIcon />
            <span>Think</span>
          </button>
          {uploading ? (
            <span className="eve-send eve-send--busy" role="status" aria-label="Sending attachments">
              <span className="eve-spinner" />
            </span>
          ) : busy && !canSend ? (
            <button
              type="button"
              className="eve-send"
              aria-label="Stop"
              title="Stop showing eve's reply"
              onClick={(e) => {
                e.stopPropagation();
                host.stop();
              }}
            >
              <StopIcon />
            </button>
          ) : (
            <button
              type="button"
              className="eve-send"
              aria-label={busy ? "Send (queued after eve's reply)" : "Send"}
              title={busy ? "Send: it goes after eve's reply" : undefined}
              disabled={!canSend}
              onClick={(e) => {
                e.stopPropagation();
                void send();
              }}
            >
              <ArrowUpIcon />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
