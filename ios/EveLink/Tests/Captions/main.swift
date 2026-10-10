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
    print(fails == 0 ? "CAPTIONS_OK" : "CAPTIONS_FAILED \(fails)")
}
await run()
