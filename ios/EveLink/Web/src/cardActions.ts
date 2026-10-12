// One outgoing card action at a time per chat, including before React observes
// isRunning. Local form edits and reset never call this function.
import { BuiltinActionType, type ActionEvent } from "@openuidev/react-lang";
import { wrapAction } from "./replyParts";

type Send = (message: { role: "user"; content: string }) => Promise<void>;
const pending = new WeakSet<Send>();

export async function dispatchCardAction(event: ActionEvent, send: Send, busy: boolean, open: (url: string) => void): Promise<string> {
  if (event.type === BuiltinActionType.OpenUrl) {
    const url = event.params?.["url"];
    if (typeof url === "string") open(url);
    return "";
  }
  if (event.type !== BuiltinActionType.ContinueConversation) return "";
  if (busy || pending.has(send)) return "Wait for Eve to finish, then tap your choice again.";
  const human = event.humanFriendlyMessage ?? "";
  const context: unknown[] = [`User clicked: ${human}`];
  // @ToAssistant(message, context): hidden data the card passes along with the tap
  if (event.params?.["context"] !== undefined) context.push(event.params["context"]);
  if (event.formState) context.push(event.formState);
  pending.add(send);
  try {
    await send({ role: "user", content: wrapAction(human, context) });
    return "";
  } catch {
    return "That choice couldn’t be sent. Try again.";
  } finally {
    pending.delete(send);
  }
}
