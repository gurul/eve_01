// Voice mode's captions on the Mac (App/VoiceLevels.swift, docs/voice-mode.md): timed to the playback clock.
import Foundation
var fails = 0
func check(_ ok: Bool, _ what: String) { print(ok ? "ok  " : "FAIL", what); if !ok { fails += 1 } }
func tone(_ secs: Double) -> [Int16] { (0..<Int(24000 * secs)).map { Int16(12000 * sin(Double($0) * 0.06)) } }
func wait(_ s: Double) async { try? await Task.sleep(for: .milliseconds(Int(s * 1000))) }

@MainActor func run() async {
    let v = VoiceLevels()
    // 1. the owner's words show at once
    v.ownerSaid("  what's on my calendar  ")
    check(v.caption == VoiceCaption(who: .owner, text: "what's on my calendar"), "owner's words at once, trimmed")
    // 2. eve: sentence one plays 1.0 s; sentence two is sent while one is still playing -> waits for it
    v.eveSaid("You have two meetings.")
    v.evePCM(tone(1.0))
    await wait(0.15)
    check(v.caption == VoiceCaption(who: .eve, text: "You have two meetings."), "eve's first sentence as it plays")
    v.eveSaid("Standup at ten.")
    v.evePCM(tone(0.8))
    await wait(0.4)
    check(v.caption?.text == "You have two meetings.", "second sentence waits while the first still plays")
    await wait(0.75)
    check(v.caption?.text == "Standup at ten.", "second sentence when its sound starts")
    // 3. fades ~4 s after eve's voice stops
    await wait(0.9 + 2.0)
    check(v.caption?.text == "Standup at ten.", "still shown 2 s after he stops")
    await wait(2.6)
    check(v.caption == nil, "gone ~4 s after he stops")
    // 4. a barge-in (flush) drops what was queued, so it never shows
    v.eveSaid("one"); v.evePCM(tone(1.0)); await wait(0.1)
    v.eveSaid("two"); v.evePCM(tone(1.0))
    v.eveFlushed()
    await wait(1.3)
    check(v.caption?.text != "two", "flushed sentence never shown")
    // 5. reset clears
    v.reset()
    check(v.caption == nil, "reset clears the caption")
    // 6. blank text is ignored
    v.ownerSaid("   "); check(v.caption == nil, "blank owner text ignored")
    // 7. eve's expression shows for its ttl, newer ids only, and expires by itself (Shared/FaceExpression.swift)
    v.showExpression(FaceExpressionRequest(id: 5, label: .happy, ttlMs: 600))
    check(v.expression == FaceExpressionShown(id: 5, label: .happy), "an expression shows at once")
    v.showExpression(FaceExpressionRequest(id: 4, label: .sad, ttlMs: 6000))
    check(v.expression?.label == .happy, "an older id is refused")
    await wait(0.3)
    check(v.expression?.label == .happy, "still shown within its ttl")
    await wait(0.45)
    check(v.expression == nil, "gone after its ttl, with nothing else live")
    v.showExpression(FaceExpressionRequest(id: 6, label: .wink, ttlMs: 4000))
    v.showExpression(FaceExpressionRequest(id: 7, label: nil))
    check(v.expression == nil, "a clear ends it")
    v.showExpression(FaceExpressionRequest(id: 8, label: .sad, ttlMs: 4000))
    v.reset()
    check(v.expression == nil, "the call's end clears it")
    v.showExpression(FaceExpressionRequest(id: 1, label: .calm, ttlMs: 4000))
    check(v.expression?.label == .calm, "a new call starts its ids afresh")
    v.reset()
    // 8. eve's audio queued ahead: a pick for a later sentence waits for that sentence's sound, ttl from there
    v.eveSaid("Bad news, the build failed."); v.evePCM(tone(1.0))
    v.eveSaid("But it's green now!"); v.evePCM(tone(1.0))
    v.showExpression(FaceExpressionRequest(id: 30, label: .sad, ttlMs: 4000, said: "Bad news, the build failed."))
    check(v.expression?.label == .sad, "a pick for the sentence playing shows at once")
    v.showExpression(FaceExpressionRequest(id: 31, label: .happy, ttlMs: 800, said: "  But it's green now! "))
    check(v.expression?.label == .sad, "a pick for the next sentence waits while the first still plays")
    v.showExpression(FaceExpressionRequest(id: 31, label: .wink, ttlMs: 800))
    check(v.expression?.label == .sad, "the id check is on arrival: a repeat of a waiting id is refused")
    await wait(0.6)
    check(v.expression?.label == .sad, "the first sentence's face holds through its sound")
    await wait(0.55)
    check(v.expression == FaceExpressionShown(id: 31, label: .happy), "the second sentence's face when its sound starts")
    await wait(0.5)
    check(v.expression?.label == .happy, "its ttl runs from its start, not from its arrival")
    await wait(0.35)
    check(v.expression == nil, "then it ends")
    // 9. a barge-in drops a pick still waiting for its sentence
    v.eveSaid("one"); v.evePCM(tone(1.0))
    v.eveSaid("two"); v.evePCM(tone(1.0))
    v.showExpression(FaceExpressionRequest(id: 40, label: .wink, ttlMs: 4000, said: "two"))
    v.eveFlushed()
    await wait(1.2)
    check(v.expression == nil, "a pick for a flushed sentence never shows")
    v.reset()
    print(fails == 0 ? "CAPTIONS_OK" : "CAPTIONS_FAILED \(fails)")
}
await run()
