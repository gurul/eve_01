// The ChatLLM AgentInterface calls when a turn starts. It never fetches:
// it asks native to POST /api/link/turn and returns a Response whose body is
// the SSE text native relays through window.eveNative.onChunk.

import {
  EventType,
  openAIAdapter,
  type AGUIEvent,
  type ChatLLM,
  type Message,
  type StreamProtocolAdapter,
} from "@openuidev/react-headless";
import { attachmentIds } from "./attachments";
import { uuid, type Bridge } from "./nativeBridge";
import { reactions as pageReactions, type ReactionStore } from "./reactions";
import { LIVE_MARK, ownerText } from "./replyParts";
import type { Ledger } from "./sync";

function lastUser(messages: Message[]): Message | undefined {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]!;
    if (m.role === "user") return m;
  }
  return undefined;
}

/**
 * openAIAdapter, with LIVE_MARK (an empty ```openui-lang fence) in front of
 * the reply's first words. OpenUI (react-ui 0.18.0)'s InterleavedTurn renders a live
 * turn's message only once its text "has lang syntax" (a fence or `root =`),
 * so a plain markdown reply stayed hidden behind the loader until the run
 * ended. splitReply drops the mark, so it never shows.
 */
export function liveMarkdownAdapter(inner: StreamProtocolAdapter = openAIAdapter()): StreamProtocolAdapter {
  return {
    async *parse(response: Response): AsyncIterable<AGUIEvent> {
      let first = true;
      for await (const event of inner.parse(response)) {
        if (first && event.type === EventType.TEXT_MESSAGE_CONTENT && event.delta) {
          first = false;
          yield { ...event, delta: LIVE_MARK + event.delta };
        } else {
          yield event;
        }
      }
    },
  };
}

export function createBridgeLLM(bridge: Bridge, ledger: Ledger, reactions: ReactionStore = pageReactions): ChatLLM {
  return {
    // The daemon streams OpenAI Chat Completions chunks
    // (data: {"choices":[{"index":0,"delta":{"content":"..."}}]} ... data: [DONE]).
    streamProtocol: liveMarkdownAdapter(),
    async send({ messages, signal }) {
      const user = lastUser(messages);
      const content = user?.content ?? "";
      const text = ownerText(content);
      const attachments = attachmentIds(content);
      const claim = ledger.beginTurn(text);
      // The send haptic is the native composer's (ChatScreen .sensoryFeedback);
      // the page plays only the "reply arrived" one, on the first real words
      // (a ": ping" keep-alive is not a reply).
      let felt = false;
      // The daemon keys this message's reaction by its client_id (the turn's id): the bubble wears it (reactions.ts).
      const clientId = uuid();
      reactions.link(user?.id, clientId);
      return bridge.openTurn({
        text,
        clientId,
        signal,
        attachments,
        hooks: {
          onBytes: (bytes) => {
            claim.bytes(bytes);
            if (!felt && claim.hasText()) {
              felt = true;
              bridge.post({ type: "haptic", style: "soft" });
            }
          },
          onEnd: claim.end,
        },
      });
    },
  };
}
