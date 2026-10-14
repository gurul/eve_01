// A turn's SSE, passed on whole events at a time and counted, so a turn whose direct link dropped mid-reply can
// carry on over HTTPS. Asking again with the same client_id joins the same turn (link_chat: DEDUPE_SECS), and its
// stream starts again from the first chunk: the events this page already showed are skipped. Only `data:` events
// are counted; ": ping" comments come at different places each time and are never counted.

const BOUNDARY = "\n\n";

export class SseEvents {
  private buffer = "";
  private toSkip: number;
  private passed = 0;

  /** `skip`: data events already shown, from an earlier try of the same turn. */
  constructor(skip = 0) {
    this.toSkip = Math.max(0, skip);
  }

  /** Data events passed on so far, the skipped ones included: what a later try skips. */
  get delivered(): number {
    return this.passed;
  }

  /** New stream text → what to pass on now (whole events only; "" while an event is still arriving). */
  push(text: string): string {
    this.buffer += text;
    let out = "";
    let at: number;
    while ((at = this.buffer.indexOf(BOUNDARY)) !== -1) {
      const event = this.buffer.slice(0, at + BOUNDARY.length);
      this.buffer = this.buffer.slice(at + BOUNDARY.length);
      out += this.take(event);
    }
    return out;
  }

  /** The stream ended: whatever is left (a last event without its blank line). */
  end(): string {
    const rest = this.buffer;
    this.buffer = "";
    return rest ? this.take(rest) : "";
  }

  private take(event: string): string {
    if (!isData(event)) return this.toSkip > 0 ? "" : event;
    this.passed += 1;
    if (this.toSkip > 0) {
      this.toSkip -= 1;
      return "";
    }
    return event;
  }
}

function isData(event: string): boolean {
  return event.split("\n").some((line) => line.startsWith("data:"));
}
