import { BuiltinActionType, type ActionEvent } from "@openuidev/react-lang";
import { describe, expect, it, vi } from "vitest";
import { dispatchCardAction } from "../cardActions";
import { ownerText } from "../replyParts";

const choice: ActionEvent = {
  type: BuiltinActionType.ContinueConversation,
  params: {},
  humanFriendlyMessage: "Plan my evening",
  formState: { evening: { place: { value: "Riverside" } } },
};

describe("card actions", () => {
  it("sends the visible choice with current form state through the existing envelope", async () => {
    const send = vi.fn(async (_message: { role: "user"; content: string }) => {});
    expect(await dispatchCardAction(choice, send, false, vi.fn())).toBe("");
    expect(send).toHaveBeenCalledOnce();
    const message = send.mock.calls[0]?.[0] as { role: string; content: string };
    expect(message.role).toBe("user");
    expect(ownerText(message.content)).toContain('"place":{"value":"Riverside"}');
    expect(ownerText(message.content)).toContain("Plan my evening");
  });

  it("passes @ToAssistant's hidden context along with the tap", async () => {
    const send = vi.fn(async (_message: { role: "user"; content: string }) => {});
    await dispatchCardAction({ ...choice, params: { context: { orderId: 42 } } }, send, false, vi.fn());
    const message = send.mock.calls[0]?.[0] as { role: string; content: string };
    expect(ownerText(message.content)).toContain('{"orderId":42}');
  });

  it("reports busy and suppresses rapid submissions across cards sharing the same chat", async () => {
    let finish!: () => void;
    const send = vi.fn((_message: {role: "user"; content: string}) => new Promise<void>((resolve) => { finish = resolve; }));
    expect(await dispatchCardAction(choice, send, true, vi.fn())).toContain("Wait for Eve");
    expect(send).not.toHaveBeenCalled();
    const first = dispatchCardAction(choice, send, false, vi.fn());
    expect(await dispatchCardAction(choice, send, false, vi.fn())).toContain("Wait for Eve");
    expect(send).toHaveBeenCalledOnce();
    finish();
    expect(await first).toBe("");
    const next = dispatchCardAction(choice, send, false, vi.fn());
    expect(send).toHaveBeenCalledTimes(2);
    finish();
    await next;
  });

  it("reports a failed send and permits retry", async () => {
    const send = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValue(undefined);
    expect(await dispatchCardAction(choice, send, false, vi.fn())).toContain("couldn’t be sent");
    expect(await dispatchCardAction(choice, send, false, vi.fn())).toBe("");
  });

  it("routes URLs to the existing opener and ignores unsupported actions", async () => {
    const send = vi.fn(async (_message: { role: "user"; content: string }) => {});
    const open = vi.fn();
    await dispatchCardAction({ type: BuiltinActionType.OpenUrl, humanFriendlyMessage: "Open", params: { url: "https://example.com" } }, send, true, open);
    expect(open).toHaveBeenCalledWith("https://example.com");
    await dispatchCardAction({ type: "other", params: {}, humanFriendlyMessage: "Other" }, send, false, open);
    expect(send).not.toHaveBeenCalled();
  });
});
