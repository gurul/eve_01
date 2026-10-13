// Eve Link's logic on the Mac: the call's PCM, the phone's turns, notes and the rendezvous (Tests/run.sh).
import Foundation

var failures = 0
func check(_ ok: Bool, _ what: String, line: Int = #line) {
    if !ok { failures += 1; FileHandle.standardError.write("line \(line): \(what)\n".data(using: .utf8)!) }
}

// Network buffers can start at any address, and the wire is explicitly little-endian.
let wirePCM = Data([0x00, 0x80, 0xff, 0xff, 0x00, 0x00, 0x01, 0x00, 0xff, 0x7f])
check(PCM16.decode(wirePCM) == [-32768, -1, 0, 1, 32767], "signed little-endian PCM")
let padded = Data([0xaa]) + wirePCM + Data([0xaa])
check(PCM16.decode(padded[1..<(padded.count - 1)]) == [-32768, -1, 0, 1, 32767], "unaligned PCM slice")
check(PCM16.decode(Data()) == [], "empty PCM")
check(PCM16.decode(Data([0])) == nil && PCM16.decode(wirePCM + Data([0])) == nil, "truncated PCM refused")

// The phone's own turns (Shared/PhoneTurn.swift, docs/action-button.md).
// URL routing: pair and talk go their ways; other schemes and hosts are refused.
let pairURL = URL(string: "evelink://pair?u=https://example.invalid&t=" + String(repeating: "x", count: 32))!
check(LinkRoute(pairURL) == .pair(pairURL), "pair link routes to pairing")
check(LinkRoute(URL(string: "evelink://talk")!) == .talk, "talk link routes to a turn")
check(LinkRoute(URL(string: "EveLink://TALK")!) == .talk, "scheme and host are case-insensitive")
check(LinkRoute(URL(string: "evelink://other")!) == nil, "unknown host refused")
check(LinkRoute(URL(string: "https://talk")!) == nil, "other scheme refused")

// One Action Button press, from each place the call can be.
check(PressAction.decide(paired: false, phoneTalking: false, eveSpeaking: false) == .notPaired, "unpaired: nothing")
check(PressAction.decide(paired: false, phoneTalking: true, eveSpeaking: true) == .notPaired, "unpaired wins")
check(PressAction.decide(paired: true, phoneTalking: false, eveSpeaking: false) == .start, "idle: start")
check(PressAction.decide(paired: true, phoneTalking: true, eveSpeaking: false) == .send, "talking: send")
check(PressAction.decide(paired: true, phoneTalking: true, eveSpeaking: true) == .send, "talking beats speaking")
check(PressAction.decide(paired: true, phoneTalking: false, eveSpeaking: true) == .bargeIn, "speaking: barge in")
// Notes on the phone (owner, 2026-10-04): while notes are on, every press stops them, whatever else is going on.
check(PressAction.decide(paired: true, takingNotes: true, phoneTalking: false, eveSpeaking: false) == .stopNotes,
      "notes: a press stops them")
check(PressAction.decide(paired: true, takingNotes: true, phoneTalking: true, eveSpeaking: false) == .stopNotes,
      "notes beat a live turn")
check(PressAction.decide(paired: true, takingNotes: true, phoneTalking: false, eveSpeaking: true) == .stopNotes,
      "notes beat eve speaking (its 'notes are on' reply)")
check(PressAction.decide(paired: false, takingNotes: true, phoneTalking: false, eveSpeaking: false) == .notPaired,
      "unpaired still wins")
check(PressAction.decide(paired: true, takingNotes: false, phoneTalking: false, eveSpeaking: false) == .start,
      "notes off: an ordinary start (control)")
check(NotesMode.on.active && NotesMode.stopping.active && !NotesMode.off.active, "notes modes")

// End of speech: 100 ms chunks at 24 kHz.
let loud = [Int16](repeating: 0, count: 2400).enumerated().map { $0.offset % 2 == 0 ? Int16(3000) : Int16(-3000) }
let quiet = [Int16](repeating: 20, count: 2400)
check(EndOfSpeech.rms(loud) > 0.05 && EndOfSpeech.rms(quiet) < 0.015, "rms tells speech from hiss")
var ear = EndOfSpeech()
var verdicts: [EndOfSpeech.Verdict] = []
for _ in 0..<10 { verdicts.append(ear.feed(loud, rate: 24_000)) }      // 1 s of speech
check(verdicts.allSatisfy { $0 == .going } && ear.heard, "speech keeps the turn open")
verdicts = []
for _ in 0..<12 { verdicts.append(ear.feed(quiet, rate: 24_000)) }     // 1.2 s of quiet
check(Array(verdicts.prefix(11)).allSatisfy { $0 == .going } && verdicts.last == .spoke, "1.2 s pause ends the turn")
var pause = EndOfSpeech()
_ = pause.feed(loud, rate: 24_000)
for _ in 0..<8 { _ = pause.feed(quiet, rate: 24_000) }
check(pause.feed(loud, rate: 24_000) == .going, "speech after a short pause resets the silence clock")
for _ in 0..<11 { check(pause.feed(quiet, rate: 24_000) == .going, "short pause after reset") }
var mute = EndOfSpeech()
var last = EndOfSpeech.Verdict.going
var chunks = 0
while last == .going && chunks < 200 { last = mute.feed(quiet, rate: 24_000); chunks += 1 }
check(last == .silent && chunks == 80, "no speech for 8 s: silent (\(chunks) chunks)")
var long = EndOfSpeech(longest: 2)
last = .going
chunks = 0
while last == .going && chunks < 200 { last = long.feed(loud, rate: 24_000); chunks += 1 }
check(last == .tooLong && chunks == 20, "the longest turn is capped (\(chunks) chunks)")
var empty = EndOfSpeech()
check(empty.feed([], rate: 24_000) == .going && EndOfSpeech.rms([]) == 0, "empty chunk")

// A held turn (Hold to talk): no pause or no-speech ending, but still capped.
var held = EndOfSpeech.held(longest: 10)          // past the 8 s no-speech limit of a self-ending turn
last = .going
chunks = 0
while last == .going && chunks < 200 { last = held.feed(quiet, rate: 24_000); chunks += 1 }
check(last == .tooLong && chunks == 100, "a held silent turn is not hung up, only capped (\(chunks) chunks)")
var spokeHeld = EndOfSpeech.held(longest: 3)
_ = spokeHeld.feed(loud, rate: 24_000)
var heldVerdicts: [EndOfSpeech.Verdict] = []
for _ in 0..<15 { heldVerdicts.append(spokeHeld.feed(quiet, rate: 24_000)) }   // 1.5 s pause after speech
check(heldVerdicts.allSatisfy { $0 == .going }, "a pause does not send a held turn")
var takenOver = EndOfSpeech(longest: 10)
for _ in 0..<50 { _ = takenOver.feed(quiet, rate: 24_000) }            // 5 s of a silent Action Button turn
takenOver.hold()
last = .going
chunks = 0
while last == .going && chunks < 200 { last = takenOver.feed(quiet, rate: 24_000); chunks += 1 }
check(last == .tooLong && chunks == 50, "a held-over turn keeps its clock (\(chunks) chunks)")

// Only a phone turn takes the microphone's audio.
check(TurnOwner.phone.takesPhoneAudio && !TurnOwner.none.takesPhoneAudio, "phone audio counts only in a phone turn")

// verification/Eve/LinkTurn.lean's counterexamples, replayed on the decisions Relay.swift takes from PhoneTurn.swift.
// Relay.holdBegan, with the turn live or not, and phoneBegan throwing or not.
func holdBegan(_ h: inout TurnHold, liveTurn: Bool, fails: Bool) {
    h.pressed()
    if liveTurn { h.tookOverLiveTurn(); return }
    h.began(endsItself: false)                                     // phoneBegan sets it before the microphone check
    if fails { h.failed() }
}
// P3(a), `p3TraceA`: press, a finger takes the turn over, the address changes, the finger lifts, the retry resumes.
var p3a = TurnHold()
p3a.began(endsItself: true)
holdBegan(&p3a, liveTurn: true, fails: false)
let ends3a = p3a.lastEndsItself
p3a.released()
check(!p3a.retryRestarts(endsItself: ends3a), "P3a: the retry does not reopen a taken-over turn after the lift")
// P3(b), `p3TraceB`: a held turn, the address changes, the finger lifts, a second hold throws (the app stops
// `holding`, so its lift calls nothing), the retry resumes.
var p3b = TurnHold()
holdBegan(&p3b, liveTurn: false, fails: false)
let ends3b = p3b.lastEndsItself
p3b.released()
holdBegan(&p3b, liveTurn: false, fails: true)
check(!p3b.holdDown && !p3b.retryRestarts(endsItself: ends3b), "P3b: a failed hold leaves no finger down")
// Control: a finger still down lets the retry restart its held turn; a turn that ends itself always restarts.
var fingerDown = TurnHold()
holdBegan(&fingerDown, liveTurn: false, fails: false)
check(fingerDown.retryRestarts(endsItself: fingerDown.lastEndsItself), "P3 control: the finger still down restarts the turn")
check(TurnHold().retryRestarts(endsItself: true), "P3 control: an Action Button turn restarts by itself")
// P4, `p4Trace`: an Action Button turn, eve's address changes (the retry starts and awaits refreshAddress), a newer
// turn begins and finishes meanwhile, then the refresh returns. The retry must not start the lost turn after the newer
// one.
func replayP4(guarded: Bool, newerTurnMeanwhile: Bool, callOpenNow: Bool = false) -> Bool {
    var gen = TurnGeneration(), hold = TurnHold(), turn = TurnOwner.none
    hold.began(endsItself: true); turn = .phone; gen.bump()          // press() → phoneBegan
    gen.bump()                                                       // fromEve(.open)
    let endsItself = hold.lastEndsItself, since = gen.count           // fromEve(.ended "address changed")
    turn = .none
    if newerTurnMeanwhile {                                          // while refreshAddress() awaits
        turn = .phone; gen.bump()                                    // press() → phoneBegan
        turn = .none                                                 // phoneEnded
    }
    guard hold.retryRestarts(endsItself: endsItself) else { return false }
    if guarded { return gen.retryMayStart(since: since, turn: turn, callOpen: callOpenNow) }
    return true                                                      // 1dd0f26: phoneBegan runs
}
check(replayP4(guarded: false, newerTurnMeanwhile: true), "P4 control: 1dd0f26 restarts the lost turn over the newer one")
check(!replayP4(guarded: true, newerTurnMeanwhile: true), "P4: a turn begun during the refresh stops the retry")
check(replayP4(guarded: true, newerTurnMeanwhile: false), "P4 control: with nothing new the retry restarts the turn")
check(!replayP4(guarded: true, newerTurnMeanwhile: false, callOpenNow: true), "P4: an open call stops the retry")
var liveTurn = TurnGeneration()
check(!liveTurn.retryMayStart(since: 0, turn: .phone, callOpen: false), "P4: a live turn stops the retry")
liveTurn.bump()
check(!liveTurn.retryMayStart(since: 0, turn: .none, callOpen: false), "P4: a call that opened stops the retry")
print("LINK_TURN_OK")

// What the intent says when it cannot start, and the Live Activity's words.
check(PhoneTurnError.notPaired.description == "Pair Eve Link first: on the Mac, run eve-bridge link pair.", "not paired text")
check(PhoneTurnError.micUnasked.description.contains("Open Eve Link"), "mic unasked text")
check(CallPhase.listening.nextPress == "Send" && CallPhase.speaking.nextPress == "Interrupt"
      && CallPhase.waiting.nextPress == "Talk", "Live Activity button words")
check(CallPhase.notes.title == "Taking notes" && CallPhase.notes.nextPress == "Stop", "notes: island words")
let notesJSON = try JSONEncoder().encode(CallPhase.notes)
check(String(decoding: notesJSON, as: UTF8.self) == "\"notes\""
      && (try? JSONDecoder().decode(CallPhase.self, from: notesJSON)) == .notes, "notes phase round-trips")

// The audio a dropped notes call holds: in order, capped at its limit with the oldest dropped, drained in frames.
var notesHeld = NotesBuffer(seconds: 1, rate: 10)   // 10 samples
notesHeld.add((0..<4).map(Int16.init))
notesHeld.add((4..<7).map(Int16.init))
check(notesHeld.pcm == (0..<7).map(Int16.init) && notesHeld.dropped == 0, "held in order under the limit")
notesHeld.add((7..<13).map(Int16.init))
check(notesHeld.pcm == (3..<13).map(Int16.init) && notesHeld.dropped == 3, "over the limit: the oldest go")
let drained = notesHeld.drain(size: 4)
check(drained.map(\.count) == [4, 4, 2] && drained.flatMap { $0 } == (3..<13).map(Int16.init), "drained in frames")
check(notesHeld.pcm.isEmpty && notesHeld.dropped == 0 && notesHeld.drain(size: 4).isEmpty, "empty after a drain")
check(NotesBuffer().limit == 60 * 24_000, "a minute at 24 kHz by default")
// A lost notes call is dialled back while notes are on or a Stop waits to go out; an end eve chose is final.
check(NotesDrop.decide(notes: .on, endedByEve: false) == .callBack, "notes on, dropped: call back")
check(NotesDrop.decide(notes: .stopping, endedByEve: false) == .callBack,
      "Stop pressed while dialling back: keep calling back, the stop and the held audio still go out")
check(NotesDrop.decide(notes: .on, endedByEve: true) == .giveUp, "eve ended it: final")
check(NotesDrop.decide(notes: .off, endedByEve: false) == .giveUp, "no notes: an ordinary end (control)")

// A notes call that hears nothing (not even a pong) for 25 s is half-open; an ordinary call is never judged.
let t0 = Date(timeIntervalSince1970: 1_000)
var alive = NotesLiveness(now: t0)
check(!alive.dead(t0.addingTimeInterval(600)), "not watched: never dead (an ordinary call is quiet)")
alive.watch(true, now: t0)
check(!alive.dead(t0.addingTimeInterval(24)), "notes: quiet under 25 s is alive")
check(alive.dead(t0.addingTimeInterval(26)), "notes: nothing for 26 s is a drop")
alive.heard(t0.addingTimeInterval(20))
check(!alive.dead(t0.addingTimeInterval(40)), "a pong keeps it alive")
alive.watch(false, now: t0.addingTimeInterval(40))
check(!alive.dead(t0.addingTimeInterval(400)), "writing up (not notes): not judged")
alive.watch(true, now: t0.addingTimeInterval(400))
check(!alive.dead(t0.addingTimeInterval(410)), "watched again: the clock starts from then")
let phaseJSON = try JSONEncoder().encode(CallPhase.thinking)
check(String(decoding: phaseJSON, as: UTF8.self) == "\"thinking\""
      && (try? JSONDecoder().decode(CallPhase.self, from: phaseJSON)) == .thinking, "phase round-trips")
print("PHONE_TURN_OK")


// ---- the rendezvous (Shared/Rendezvous.swift): the topic must equal the daemon's ----------------------------
// Vector from Python: stick_link.rendezvous_topic("a" * 40).
check(Rendezvous.topic(token: String(repeating: "a", count: 40)) == "evelink-a7c4bc0e3e702a6abc8c7afe80e62b5d",
      "rendezvous topic matches the daemon's")
let ntfyBody = """
{"id":"1","time":1,"event":"open","topic":"t"}
{"id":"2","time":2,"event":"message","topic":"t","message":"https://old-name.trycloudflare.com"}
{"id":"3","time":3,"event":"message","topic":"t","message":"https://evil.example.com"}
{"id":"4","time":4,"event":"message","topic":"t","message":"https://new-name-here.trycloudflare.com"}
{"id":"5","time":5,"event":"message","topic":"t","message":"https://x.trycloudflare.com/path"}
"""
check(Rendezvous.newest(in: ntfyBody)?.absoluteString == "https://new-name-here.trycloudflare.com",
      "newest believable address wins; other hosts and paths are ignored")
check(Rendezvous.newest(in: "") == nil, "empty topic gives nothing")
check(!Rendezvous.isTunnel("http://a.trycloudflare.com") && Rendezvous.isTunnel("https://a-b-1.trycloudflare.com"),
      "only https quick-tunnel origins are believed")
// Notifications (Shared/Rendezvous.swift, NotifyTopic): vector from Python, notify.notify_topic("a" * 40).
check(NotifyTopic.topic(token: String(repeating: "a", count: 40)) == "eve-312f09f108bc0795afe2c8447ddfbd6a",
      "notify topic matches the daemon's")
check(NotifyTopic.topic(token: String(repeating: "a", count: 40)) != Rendezvous.topic(token: String(repeating: "a", count: 40)),
      "notify and rendezvous topics differ")
check(LinkRoute(URL(string: "evelink://chat")!) == .chat, "a notification's tap routes to the Chat")
print("RENDEZVOUS_OK")

// ---- the chat bridge (Shared/ChatBridge.swift, Shared/ChatTransport.swift, docs/app-chat.md) ------------------
// JS literals: whatever eve says stays a string. Each literal must parse back to the same text as JSON.
let nasty = "a\"b\\c\nd\re\tf\u{0}\u{1f}\u{7f}</script>\u{2028}\u{2029}é😀';alert(1)//"
let nastyLit = ChatJS.literal(nasty)
let nastyBack = try? JSONSerialization.jsonObject(with: Data(nastyLit.utf8), options: .fragmentsAllowed) as? String
check(nastyBack == nasty, "a JS literal decodes back to the same text")
check(!nastyLit.contains("\n") && !nastyLit.contains("\r") && !nastyLit.contains("\u{2028}") && !nastyLit.contains("</"),
      "no raw line breaks or </ inside a literal")
check(nastyLit.dropFirst().dropLast().replacingOccurrences(of: "\\\"", with: "").contains("\"") == false,
      "every quote inside is escaped")
check(ChatJS.literal("") == "\"\"" && ChatJS.literal(true) == "true", "empty string and bool literals")
let callJS = ChatJS.call("onChunk", [ChatJS.literal("t1"), ChatJS.literal("x")])
check(callJS.contains("n.onChunk(\"t1\",\"x\")") && callJS.contains("typeof n.onChunk==='function'"),
      "calls go through window.eveNative, guarded")

// Message dispatch: each JS -> native message, and what is refused.
check(ChatBridgeMessage(body: ["type": "ready"]) == .ready, "ready")
check(ChatBridgeMessage(body: ["type": "topics", "id": "q1"]) == .topics(id: "q1"), "topics")
check(ChatBridgeMessage(body: ["type": "history", "open": true]) == .history(open: true), "history open")
check(ChatBridgeMessage(body: ["type": "history", "open": 1]) == nil, "history: open must be a bool")
check(ChatBridgeMessage(body: ["type": "bookmark", "id": "q2", "topic": "2026-10-13:12", "on": true])
      == .bookmark(id: "q2", topic: "2026-10-13:12", on: true), "bookmark")
check(ChatBridgeMessage(body: ["type": "bookmark", "id": "q2", "topic": "../x", "on": true]) == nil,
      "bookmark: a topic id that is not <day>:<line> is refused")
check(ChatBridgeMessage(body: ["type": "bookmark", "id": "q2", "topic": "2026-10-13:12", "on": "yes"]) == nil,
      "bookmark: on must be a bool")
check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": "  hi there \n", "client_id": "c1"])
      == .turn(id: "t1", text: "hi there", clientID: "c1"), "turn, text trimmed")
check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": "   ", "client_id": "c1"]) == nil, "blank turn refused")
check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": String(repeating: "x", count: 4001), "client_id": "c"])
      == nil, "a turn over 4000 characters refused")
check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": String(repeating: "x", count: 4000), "client_id": "c"])
      != nil, "4000 characters allowed (control)")
check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": "hi", "client_id": String(repeating: "c", count: 65)])
      == nil, "client_id over 64 refused")
check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": "hi"]) == nil, "turn without client_id refused")
check(ChatBridgeMessage(body: ["type": "cancel", "id": "t1"]) == .cancel(id: "t1"), "cancel")
check(ChatBridgeMessage(body: ["type": "cancel"]) == nil, "cancel without id refused")
check(ChatBridgeMessage(body: ["type": "messages", "id": "m1", "after": "c9", "wait": NSNumber(value: 25)])
      == .messages(id: "m1", after: "c9", wait: 25), "messages with a cursor")
check(ChatBridgeMessage(body: ["type": "messages", "id": "m1", "wait": NSNumber(value: 99)])
      == .messages(id: "m1", after: nil, wait: 25), "wait capped at 25, no cursor")
check(ChatBridgeMessage(body: ["type": "messages", "id": "m1", "after": NSNull(), "wait": NSNumber(value: -3)])
      == .messages(id: "m1", after: nil, wait: 0), "negative wait is 0, null cursor is none")
check(ChatBridgeMessage(body: ["type": "haptic", "style": "success"]) == .haptic(.success), "haptic")
check(ChatBridgeMessage(body: ["type": "haptic", "style": "heavy"]) == nil, "unknown haptic refused")
check(ChatBridgeMessage(body: ["type": "openURL", "url": "https://example.com/a"])
      == .openURL(URL(string: "https://example.com/a")!), "openURL https")
check(ChatBridgeMessage(body: ["type": "openURL", "url": "javascript:alert(1)"]) == nil, "openURL javascript: refused")
check(ChatBridgeMessage(body: ["type": "openURL", "url": "file:///etc/passwd"]) == nil, "openURL file: refused")
check(ChatBridgeMessage(body: ["type": "openURL", "url": "evelink://pair?u=x"]) == nil, "openURL to the app refused")
for link in ["maps://?q=coffee", "facetime:+15551234567", "facetime-audio:+15551234567", "tel:+15551234567"] {
    check(ChatBridgeMessage(body: ["type": "openURL", "url": link]) == .openURL(URL(string: link)!)
          && ChatLinks.external(URL(string: link)!) == .system, "openURL \(link) goes to its app, as links.ts allows")
}
// The 4000 limit counts Unicode scalars, as the daemon's len() does: 2000 flags are 2000 Characters but 4000 scalars.
let flags = String(repeating: "\u{1F1EF}\u{1F1F5}", count: 2000)
check(flags.count == 2000 && !ChatBridgeMessage.tooLong(flags)
      && ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": flags, "client_id": "c"]) != nil,
      "4000 scalars pass")
let flagsOver = flags + "\u{1F1EF}"
check(flagsOver.count == 2001 && ChatBridgeMessage.tooLong(flagsOver)
      && ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": flagsOver, "client_id": "c"]) == nil,
      "4001 scalars refused though only 2001 Characters")
let combining = String(repeating: "e\u{301}", count: 2001)
check(combining.count == 2001 && ChatBridgeMessage.tooLong(combining), "combining marks count as the daemon counts them")
check(ChatBridgeMessage(body: ["type": "eval", "id": "x"]) == nil && ChatBridgeMessage(body: "ready") == nil
      && ChatBridgeMessage(body: ["id": "x"]) == nil, "unknown types and shapes dropped")

// Navigation: only the bundled page; any other URL is refused, and a tapped link opens outside.
check(ChatNavigation.decide(URL(string: "eveapp://chat/index.html"), linkActivated: false) == .allow, "bundled page")
check(ChatNavigation.decide(URL(string: "eveapp://chat/index.html#x"), linkActivated: true) == .allow, "in-page anchor")
for bad in ["https://example.com", "http://127.0.0.1:8080/", "about:blank", "about:srcdoc", "data:text/html,hi",
            "blob:eveapp://chat/1", "file:///etc/hosts", "javascript:alert(1)", "eveapp://evil/index.html",
            "evelink://pair?u=x"] {
    check(ChatNavigation.decide(URL(string: bad), linkActivated: false) == .refuse, "refused: \(bad)")
}
check(ChatNavigation.decide(URL(string: "https://example.com/x"), linkActivated: true)
      == .open(URL(string: "https://example.com/x")!), "a tapped https link opens in Safari")
check(ChatNavigation.decide(URL(string: "javascript:alert(1)"), linkActivated: true) == .refuse,
      "a tapped javascript: link is refused")
check(ChatNavigation.decide(nil, linkActivated: false) == .refuse, "no URL refused")

// Bundled files: inside ChatWeb only.
let assetRoot = URL(fileURLWithPath: "/bundle/ChatWeb")
let anyFile: (URL) -> Bool = { _ in true }
check(ChatAssets.resolve(URL(string: "eveapp://chat/index.html")!, root: assetRoot, exists: anyFile)?.path
      == "/bundle/ChatWeb/index.html", "index.html")
check(ChatAssets.resolve(URL(string: "eveapp://chat/")!, root: assetRoot, exists: anyFile)?.path
      == "/bundle/ChatWeb/index.html", "the root is index.html")
check(ChatAssets.resolve(URL(string: "eveapp://chat/assets/index-abc.js")!, root: assetRoot, exists: anyFile)?.path
      == "/bundle/ChatWeb/assets/index-abc.js", "a file in a subfolder")
check(ChatAssets.resolve(URL(string: "eveapp://chat/../Info.plist")!, root: assetRoot, exists: anyFile) == nil
      && ChatAssets.resolve(URL(string: "eveapp://chat/assets/%2E%2E/%2E%2E/x")!, root: assetRoot, exists: anyFile) == nil
      && ChatAssets.resolve(URL(string: "eveapp://chat/.hidden")!, root: assetRoot, exists: anyFile) == nil,
      "nothing outside ChatWeb, no dot files")
check(ChatAssets.resolve(URL(string: "eveapp://other/index.html")!, root: assetRoot, exists: anyFile) == nil
      && ChatAssets.resolve(URL(string: "https://chat/index.html")!, root: assetRoot, exists: anyFile) == nil,
      "other hosts and schemes")
check(ChatAssets.resolve(URL(string: "eveapp://chat/missing.js")!, root: assetRoot, exists: { _ in false }) == nil,
      "a missing file")
check(ChatAssets.mimeType(URL(fileURLWithPath: "/a/x.js")).hasPrefix("text/javascript")
      && ChatAssets.mimeType(URL(fileURLWithPath: "/a/x.css")).hasPrefix("text/css")
      && ChatAssets.mimeType(URL(fileURLWithPath: "/a/index.html")).hasPrefix("text/html"), "content types")
check(ChatAssets.contentSecurityPolicy.contains("connect-src 'none'"), "the page cannot fetch")

// Requests: the token rides in the Authorization header only.
let ep = ChatEndpoint(base: URL(string: "https://a-b.trycloudflare.com")!, token: String(repeating: "k", count: 40))
let turnReq = ChatRequests.turn(ep, text: "hi", clientID: "c1")
let turnBody = (try? JSONSerialization.jsonObject(with: turnReq.httpBody ?? Data())) as? [String: String]
check(turnReq.httpMethod == "POST" && turnReq.url?.absoluteString == "https://a-b.trycloudflare.com/api/link/turn"
      && turnReq.value(forHTTPHeaderField: "Authorization") == "Bearer " + ep.token
      && turnReq.value(forHTTPHeaderField: "Origin") == nil
      && turnBody == ["text": "hi", "client_id": "c1", "grade": "auto"], "the turn request (auto by default)")
for g in ChatGrade.allCases {
    let body = (try? JSONSerialization.jsonObject(with: ChatRequests.turn(ep, text: "hi", clientID: "c1", grade: g)
        .httpBody ?? Data())) as? [String: String]
    check(body?["grade"] == g.rawValue && body?["text"] == "hi", "the speed dial's \(g.rawValue) rides in the turn")
}
check(ChatGrade.allCases.map(\.rawValue) == ["auto", "instant", "think"], "the three grades the daemon takes")
check(ChatGrade.stored(nil) == .auto && ChatGrade.stored("think") == .think && ChatGrade.stored("fast") == .auto,
      "the remembered grade: unknown or none is auto")
let msgReq = ChatRequests.messages(ep, after: "c 9&x", wait: 25)
check(msgReq.httpMethod == "GET" && msgReq.url?.path == "/api/link/messages"
      && URLComponents(url: msgReq.url!, resolvingAgainstBaseURL: false)?.queryItems
         == [URLQueryItem(name: "after", value: "c 9&x"), URLQueryItem(name: "wait", value: "25")]
      && !(msgReq.url?.absoluteString.contains(ep.token) ?? true) && msgReq.timeoutInterval > 25, "the messages request")
check(ChatRequests.messages(ep, after: nil, wait: 0).url?.query == nil, "first page: no query")

// Failures: which ones look up a new address.
check(ChatFailure.from(URLError(.cannotFindHost)).addressMayHaveMoved
      && ChatFailure.from(status: 530, body: Data()).addressMayHaveMoved, "a gone tunnel retries at a new address")
check(!ChatFailure.from(status: 401, body: Data()).addressMayHaveMoved
      && !ChatFailure.from(status: 503, body: Data()).addressMayHaveMoved
      && !ChatFailure.from(URLError(.cancelled)).addressMayHaveMoved, "401, 503 and a cancel do not")
check(ChatFailure.from(status: 503, body: Data(#"{"ok":false,"error":"no chat"}"#.utf8)).message == "no chat",
      "503 shows the daemon's words")

// UTF-8 cut at any byte comes out whole.
let words = Array("aé€😀b".utf8)
for cut in 0...words.count {
    var carry: [UInt8] = []
    let joined = UTF8Carry.take(&carry, Array(words[..<cut])) + UTF8Carry.take(&carry, Array(words[cut...]))
    check(joined == "aé€😀b" && carry.isEmpty, "utf-8 cut at byte \(cut)")
}

// SSE forwarding: bytes in, the same text out, in order, before the end; batched when they come together.
let sse = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"héllo 😀\"}}]}\n\n: ping\n\n" +
    String(repeating: "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}\n\n", count: 200) + "data: [DONE]\n\n"
let deliveryQueue = DispatchQueue(label: "chat-test")
final class Collected: @unchecked Sendable {
    let lock = NSLock(); var parts: [String] = []; var endedAfter = -1
}
let collected = Collected()
let forwarder = ByteForwarder(schedule: { work in deliveryQueue.async(execute: work) },
                              deliver: { s in collected.lock.withLock { collected.parts.append(s) } })
let byteStream = AsyncStream<UInt8> { c in
    for b in Array(sse.utf8) { c.yield(b) }
    c.finish()
}
try await ByteForwarder.pump(byteStream, into: forwarder)
await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
    forwarder.end { collected.lock.withLock { collected.endedAfter = collected.parts.count }; c.resume() }
}
check(collected.parts.joined() == sse, "forwarded text equals the stream")
check(collected.endedAfter == collected.parts.count && collected.parts.count >= 1, "the end comes after every chunk")
check(collected.parts.count < sse.split(separator: "\n", omittingEmptySubsequences: false).count,
      "chunks are batched (\(collected.parts.count) deliveries)")
check(forwarder.bytesPushed == sse.utf8.count, "every byte counted")
// Split mid-character across pushes, delivered one push per flush.
let slow = Collected()
let one = ByteForwarder(schedule: { work in deliveryQueue.sync(execute: work) },
                        deliver: { s in slow.parts.append(s) })
let emoji = Array("😀\n".utf8)
one.push([emoji[0], emoji[1]]); one.push([emoji[2]]); one.push([emoji[3], emoji[4]])
one.end {}
check(slow.parts == ["😀\n"], "a character split over three pushes arrives whole (\(slow.parts))")
let cutShort = Collected()
let short = ByteForwarder(schedule: { work in deliveryQueue.sync(execute: work) }, deliver: { cutShort.parts.append($0) })
short.push([0x61, 0xF0, 0x9F]); short.end {}
check(cutShort.parts.joined().hasPrefix("a") && cutShort.parts.joined().count == 2, "a cut-off end is still delivered")
print("CHAT_BRIDGE_OK")

// The Work view (Shared/WorkClient.swift): jobs and apps from the daemon.
let jobsJSON = """
{"jobs": [
  {"id": "c12", "kind": "job", "title": "Book the dentist", "status": "waiting", "waiting_for": "go",
   "end_state": "An appointment next week", "deadline": null, "cue": null,
   "phases": [{"n": 2, "do": "act", "goal": "Book it", "door": "one_way", "status": "queued", "executor": "jev", "why": ""},
              {"n": 1, "do": "research", "goal": "Find a slot", "door": "two_way", "status": "ok", "executor": "jev", "why": ""}],
   "checks": [], "spent": {"usd": 0.12, "minutes": 3.5}, "budget": {"usd": 1.0, "minutes": 30},
   "needs_you": true, "actions": ["go", "drop", "launch"]},
  {"id": "c13", "kind": "job", "title": "Compare flights", "status": "active", "waiting_for": null,
   "end_state": "", "phases": [{"n": 1, "do": "research", "goal": "Search", "door": "two_way", "status": "running"}],
   "needs_you": false, "actions": ["done", "drop"]},
  {"id": "c9", "kind": "commitment", "title": "Send the deck", "status": "done", "phases": [],
   "needs_you": false, "actions": ["hide"]},
  {"title": "no id: dropped"}
], "needs_you": 1}
"""
let jobs = WorkJobs.decode(Data(jobsJSON.utf8))
check(jobs?.jobs.map(\.id) == ["c12", "c13", "c9"] && jobs?.needsYou == 1, "jobs decode; a card without an id is left out")
if let j = jobs?.jobs.first {
    check(j.steps.map(\.n) == [1, 2] && j.steps[1].oneWay && !j.steps[0].oneWay, "steps in order, one-way marked")
    check(j.actions == [.go, .drop], "only the actions the app knows (an unknown one is left out)")
    check(j.needsYou && j.statusLine == "Waiting on your Go", "waiting on a Go")
    check(j.stepsDone == 1 && j.progress == 0.5, "one of two steps done")
    check(j.spentUSD == 0.12 && j.budgetUSD == 1.0 && j.endState == "An appointment next week", "spent and budget")
    check(j.steps[1].statusWord == "Needs your Go" && j.steps[0].statusWord == "Done", "step words")
}
check(jobs?.jobs[1].statusLine == "Researching" && jobs?.jobs[1].progress == 0, "a running research step")
let toldJSON = #"{"jobs": [{"id": "c1", "title": "x", "status": "active", "line": "May have happened: check","#
    + #" "steps_done": 1, "phases": [{"n": 1, "status": "failed"}, {"n": 2, "status": "ok"}]}]}"#
let told = WorkJobs.decode(Data(toldJSON.utf8))?.jobs.first
check(told?.statusLine == "May have happened: check" && told?.stepsDone == 1 && told?.progress == 0.5,
      "the daemon's own line and steps done win")
check(jobs?.jobs[2].statusLine == "Done" && jobs?.jobs[2].progress == 1 && jobs?.jobs[2].open == false, "a finished job")
check(jobs?.open.map(\.id) == ["c12", "c13"] && jobs?.finished.map(\.id) == ["c9"], "open first, then finished")
check(jobs?.finished.first?.actions == [.hide] && WorkAction.hide.title == "Delete", "a finished job can be deleted")
check(WorkJobs.decode(Data(#"{"jobs": [{"id": "c1", "title": "x", "needs_you": true}]}"#.utf8))?.needsYou == 1,
      "no needs_you count: counted from the jobs")
check(WorkJobs.decode(Data("[]".utf8)) == nil && WorkJobs.decode(Data("nope".utf8)) == nil, "a bad payload is nil")
for (status, waiting, line) in [("waiting", "question", "Waiting on your answer"), ("waiting", "budget", "Waiting on a budget raise"),
                                ("waiting", "mac_slot", "Waiting for the Mac"), ("proposed", nil, "Proposed"),
                                ("dropped", nil, "Dropped"), ("active", nil, "Working")] {
    check(WorkJob(id: "c1", title: "t", status: status, waitingFor: waiting).statusLine == line, "status line: \(line)")
}

let appsJSON = #"{"apps": [{"slug": "water", "name": "Water", "updated": "2026-10-08T11:50:02"},"#
    + #" {"slug": "notes", "name": "", "updated": ""}, {"name": "no slug"}]}"#
let apps = WorkApp.decodeList(Data(appsJSON.utf8))
check(apps?.map(\.slug) == ["water", "notes"] && apps?[1].name == "notes", "apps decode; no name shows the slug")
let parts = apps?[0].updatedDate.map { Calendar.current.dateComponents([.year, .month, .day, .hour, .minute], from: $0) }
check(parts?.year == 2026 && parts?.month == 10 && parts?.day == 8 && parts?.hour == 11 && parts?.minute == 50,
      "the app store's local time reads as local")
check(apps?[1].updatedDate == nil && WorkApp.parse("2026-10-08T09:00:00Z") != nil, "no time, and a zoned time")
let tunnel = URL(string: "https://a-b.trycloudflare.com")!
check(WorkApp.decodeOpen(Data(#"{"url": "https://a-b.trycloudflare.com/apps/water/?t=abc"}"#.utf8), base: tunnel)
      == URL(string: "https://a-b.trycloudflare.com/apps/water/?t=abc"), "the open URL on eve's tunnel")
check(WorkApp.decodeOpen(Data(#"{"url": "https://evil.example/apps/water/?t=abc"}"#.utf8), base: tunnel) == nil
      && WorkApp.decodeOpen(Data(#"{"url": "javascript:alert(1)"}"#.utf8), base: tunnel) == nil
      && WorkApp.decodeOpen(Data(#"{"url": "http://a-b.trycloudflare.com/apps/x/"}"#.utf8), base: tunnel) == nil,
      "an open URL anywhere else is refused")

check(WorkActionResult.decode(Data(#"{"ok": true, "id": "c12"}"#.utf8)) == WorkActionResult(ok: true, reason: nil)
      && WorkActionResult.decode(Data(#"{"ok": false, "reason": "there is no open card c1"}"#.utf8))
         == WorkActionResult(ok: false, reason: "there is no open card c1")
      && WorkActionResult.decode(Data("x".utf8)).ok == false, "action results")

let jobsReq = WorkRequests.jobs(ep)
check(jobsReq.httpMethod == "GET" && jobsReq.url?.absoluteString == "https://a-b.trycloudflare.com/api/link/jobs"
      && jobsReq.value(forHTTPHeaderField: "Authorization") == "Bearer " + ep.token
      && jobsReq.value(forHTTPHeaderField: "Origin") == nil, "the jobs request")
for action in WorkAction.allCases {
    let r = WorkRequests.act(ep, job: "c12", action)
    check(r.httpMethod == "POST" && r.url?.path == "/api/link/jobs/c12/\(action.rawValue)"
          && r.value(forHTTPHeaderField: "Authorization") == "Bearer " + ep.token
          && !(r.url?.absoluteString.contains(ep.token) ?? true), "the \(action.rawValue) request")
}
check(WorkAction.allCases.map(\.rawValue) == ["go", "done", "drop", "raise", "happened", "didnt", "hide"],
      "the seven actions the daemon takes")
let goReq = WorkRequests.act(ep, job: "c12", .go, rev: 3)
check(goReq.httpBody.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Int] } == ["rev": 3],
      "a Go carries the revision the sheet showed")
check(WorkRequests.act(ep, job: "c12", .drop, rev: 3).httpBody == Data("{}".utf8)
      && WorkRequests.act(ep, job: "c12", .go).httpBody == Data("{}".utf8), "only a Go with a revision sends one")
let doubtJSON = #"{"jobs": [{"id": "c1", "title": "x", "rev": 2, "actions": ["happened", "didnt", "drop"]}]}"#
let doubtJob = WorkJobs.decode(Data(doubtJSON.utf8))?.jobs.first
check(doubtJob?.rev == 2 && doubtJob?.actions == [.happened, .didnt, .drop], "an act in doubt: its two answers, no Go")
check(WorkRequests.apps(ep).url?.path == "/api/link/apps" && WorkRequests.apps(ep).httpMethod == "GET", "the apps request")
let openReq = WorkRequests.openApp(ep, slug: "water")
check(openReq.httpMethod == "POST" && openReq.url?.path == "/api/link/apps/water/open"
      && openReq.value(forHTTPHeaderField: "Authorization") == "Bearer " + ep.token, "the open request")
check(WorkRequests.openApp(ep, slug: "a/../b").url?.absoluteString
      == "https://a-b.trycloudflare.com/api/link/apps/a%2F..%2Fb/open", "a slug cannot climb the path")
check(WorkFailure.from(status: 503, body: Data(), for: .jobs).message == "eve's chief is off."
      && WorkFailure.from(status: 503, body: Data(), for: .apps).message == "eve's app builder is off."
      && WorkFailure.from(status: 404, body: Data(), for: .apps).message == "That isn't there any more."
      && WorkFailure.from(status: 530, body: Data(), for: .jobs).addressMayHaveMoved, "work failures")
// New job (owner, 2026-10-09): the request, and what it came to.
let startReq = WorkRequests.start(ep, goal: "  Plan my trip to Rome \n", detail: "  under $200 a night ", clientID: "job-1")
let startBody = startReq.httpBody.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: String] }
check(startReq.httpMethod == "POST" && startReq.url?.path == "/api/link/jobs"
      && startReq.value(forHTTPHeaderField: "Authorization") == "Bearer " + ep.token
      && startReq.value(forHTTPHeaderField: "Origin") == nil && startReq.timeoutInterval == WorkRequests.startTimeout
      && startBody == ["goal": "Plan my trip to Rome", "detail": "under $200 a night", "client_id": "job-1"],
      "the New job request: trimmed, with its client id, and a long wait")
let bareBody = WorkRequests.start(ep, goal: "x", detail: "   ", clientID: "job-2").httpBody
    .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: String] }
check(bareBody == ["goal": "x", "client_id": "job-2"], "no details: no detail key")
let longBody = WorkRequests.start(ep, goal: String(repeating: "g", count: 1500), detail: String(repeating: "d", count: 2500),
                                  clientID: "job-3").httpBody
    .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: String] }
check(longBody?["goal"]?.count == WorkRequests.maxGoal && longBody?["detail"]?.count == WorkRequests.maxDetail,
      "the goal and details are cut to the daemon's limits")
let startedJSON = #"{"ok": true, "said": ["On it."], "job": {"id": "c7", "title": "Rome trip", "status": "active", "rev": 1, "phases": [{"n": 1, "do": "research", "goal": "flights", "door": null, "status": "queued"}], "needs_you": false, "actions": ["drop"]}}"#
let startedJob = WorkStartResult.decode(Data(startedJSON.utf8))
check(startedJob.job?.id == "c7" && startedJob.job?.title == "Rome trip" && startedJob.job?.steps.count == 1
      && startedJob.job?.actions == [.drop] && startedJob.said == ["On it."], "a New job's card decodes as a listed one")
let unfiledStart = WorkStartResult.decode(Data(#"{"ok": false, "reason": "eve answered without starting a job", "said": ["Which city?", ""]}"#.utf8))
check(unfiledStart.job == nil && unfiledStart.said == ["Which city?"] && unfiledStart.problem == "Which city?",
      "no card: eve's own words are the sheet's line")
let quietStart = WorkStartResult.decode(Data(#"{"ok": false, "reason": "eve is still on it", "said": []}"#.utf8))
check(quietStart.job == nil && quietStart.problem == "eve is still on it", "no card and no words: the reason")
check(WorkStartResult.decode(Data(#"{"ok": false, "job": {"id": "c1", "title": "x"}}"#.utf8)).job == nil
      && WorkStartResult.decode(Data(#"{"ok": true, "job": {"title": "no id"}}"#.utf8)).job == nil
      && WorkStartResult.decode(Data("nope".utf8)).problem == "eve didn't start a job.",
      "a card only with ok and an id")
print("WORK_CLIENT_OK")
// Voice mode's hands-free call (Shared/PhoneTurn.swift, docs/voice-mode.md).
let speech = [Int16](repeating: 4000, count: 960)        // 40 ms, RMS about 0.12
let hush = [Int16](repeating: 60, count: 960)            // 40 ms of room hiss, RMS about 0.002
var onset = SpeechOnset()
var fired: [Int16]?
for _ in 0..<30 where fired == nil { fired = onset.feed(hush) }      // 1.2 s of hiss: nothing goes to eve
check(fired == nil && onset.held.count == 12_000, "hiss never starts a turn; only the last 0.5 s is held")
check(onset.feed(speech) == nil, "one 40 ms burst is not speech yet")
check(onset.feed(hush) == nil && onset.feed(speech) == nil, "a burst after a gap starts the count again")
check(onset.feed(speech) == nil && onset.feed(speech) == nil, "120 ms of voice is not enough")
let started = onset.feed(speech)
check(started?.count == 12_000 && started?.suffix(3840) == ArraySlice(speech + speech + speech + speech),
      "150 ms of voice: the turn starts with the 0.5 s before it, ending on the words")
check(onset.held.isEmpty, "nothing is held after the turn starts")
var clicky = SpeechOnset()
var clickChunk = [Int16](repeating: 60, count: 2400)    // 100 ms of hiss with a 5 ms full-scale click in it
for i in 1000..<1120 { clickChunk[i] = 32_767 }
check(clicky.feed(clickChunk) == nil && clicky.feed(clickChunk) == nil && clicky.feed(clickChunk) == nil,
      "a 5 ms click in a 100 ms chunk is not speech, however loud (10 ms frames, not chunks)")
var longChunks = SpeechOnset()
let voice100 = [Int16](repeating: 4000, count: 2400)
check(longChunks.feed(voice100) == nil && longChunks.feed(voice100)?.count == 4800,
      "100 ms chunks: one is not enough, the second reaches 150 ms")
var split = SpeechOnset()
check(split.feed(Array(speech.prefix(100))) == nil && split.feed(speech + speech + speech) == nil
      && split.feed(speech) != nil, "frames carry across chunk edges")
var taken = SpeechOnset()
_ = taken.feed(hush)
check(taken.drain() == hush && taken.held.isEmpty, "a finger taking the turn gets what was held")
var emptyOnset = SpeechOnset()
check(emptyOnset.feed([]) == nil && emptyOnset.held.isEmpty, "empty chunk")
check(HandsFree.startsNow(phase: .ended, phoneTalking: false, takingNotes: false)
      && HandsFree.startsNow(phase: .waiting, phoneTalking: false, takingNotes: false), "voice mode opens: listen")
check(!HandsFree.startsNow(phase: .thinking, phoneTalking: false, takingNotes: false)
      && !HandsFree.startsNow(phase: .speaking, phoneTalking: false, takingNotes: false),
      "eve is answering: the mic opens when he is done, not over him")
check(!HandsFree.startsNow(phase: .listening, phoneTalking: true, takingNotes: false)
      && !HandsFree.startsNow(phase: .notes, phoneTalking: false, takingNotes: true),
      "a turn already open, or notes: leave them")
check(HandsFree.reopens(handsFree: true, replyToPhone: true, phoneTalking: false, takingNotes: false, callOpen: true),
      "eve is done: the mic opens again")
check(!HandsFree.reopens(handsFree: false, replyToPhone: true, phoneTalking: false, takingNotes: false, callOpen: true),
      "voice mode closed (the Action Button's calls): never reopens by itself")
check(!HandsFree.reopens(handsFree: true, replyToPhone: true, phoneTalking: true, takingNotes: false, callOpen: true)
      && !HandsFree.reopens(handsFree: true, replyToPhone: true, phoneTalking: false, takingNotes: true, callOpen: true)
      && !HandsFree.reopens(handsFree: true, replyToPhone: true, phoneTalking: false, takingNotes: false,
                            callOpen: false)
      && !HandsFree.reopens(handsFree: true, replyToPhone: false, phoneTalking: false, takingNotes: false,
                            callOpen: true), "not over a turn, notes, a closed call, or a call that is not the phone's")
check(VoiceStatus.text(phase: .listening, connected: false, notes: .off, error: nil) == "Connecting…"
      && VoiceStatus.text(phase: .listening, connected: true, notes: .off, error: nil) == "Listening"
      && VoiceStatus.text(phase: .waiting, connected: true, notes: .off, error: nil) == "Listening"
      && VoiceStatus.text(phase: .speaking, connected: true, notes: .off, error: nil) == "eve is speaking"
      && VoiceStatus.text(phase: .thinking, connected: true, notes: .off, error: nil) == "eve is thinking…"
      && VoiceStatus.text(phase: .ended, connected: false, notes: .off, error: nil) == "Connecting…",
      "voice status from the call's phase")
check(VoiceStatus.text(phase: .ended, connected: false, notes: .off, error: "eve's chat isn't running")
      == "eve's chat isn't running"
      && VoiceStatus.text(phase: .listening, connected: true, notes: .off, error: "") == "Listening",
      "an error wins; an empty one is none")
check(VoiceStatus.text(phase: .notes, connected: true, notes: .on, error: nil) == "Taking notes"
      && VoiceStatus.text(phase: .thinking, connected: true, notes: .stopping, error: nil) == "Writing up your notes…",
      "notes")
check(ChatJS.insets(top: 103.26, bottom: 92) == "{top:103.3,bottom:92.0}"
      && ChatJS.insets(top: -4, bottom: .nan) == "{top:0.0,bottom:0.0}"
      && ChatJS.insets(top: .infinity, bottom: 1) == "{top:0.0,bottom:1.0}", "setInsets numbers only, finite, ≥ 0")
check(ChatJS.call("setInsets", [ChatJS.insets(top: 1, bottom: 2)]).contains("n.setInsets({top:1.0,bottom:2.0})"),
      "setInsets call")
print("VOICE_MODE_OK")

// Attachments (docs/app-chat.md, "Attachments"): the ids a turn may carry, the upload request, the daemon's answer.
let upID = "0123456789abcdef0123456789abcdef"
check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": " look ", "client_id": "c1", "attachments": [upID]])
      == .turn(id: "t1", text: "look", clientID: "c1", attachments: [upID]), "turn with an attachment")
check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": "", "client_id": "c1", "attachments": [upID]])
      == .turn(id: "t1", text: "", clientID: "c1", attachments: [upID]), "an attachment alone is a turn")
check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": "", "client_id": "c1", "attachments": [String]()])
      == nil, "no words and nothing attached is refused")
for bad: Any in [["../../etc/passwd"], [upID.uppercased()], [upID, upID], Array(repeating: upID, count: 5),
                 [String(upID.dropLast())], "nope", [7]] {
    check(ChatBridgeMessage(body: ["type": "turn", "id": "t1", "text": "x", "client_id": "c1", "attachments": bad])
          == nil, "attachments refused: \(bad)")
}
let fourIDs = (0..<4).map { String(repeating: String($0), count: 32) }
check(ChatAttachments.valid(fourIDs) && !ChatAttachments.valid(fourIDs + ["a" + String(repeating: "0", count: 31)]),
      "four ids pass, five do not")
let upEP = ChatEndpoint(base: URL(string: "https://x.trycloudflare.com")!, token: "tok")
let upReq = ChatRequests.upload(upEP, data: Data([1, 2, 3]), name: "Beach day/ü.jpg", mime: "image/jpeg")
check(upReq.httpMethod == "POST" && upReq.url?.path == "/api/link/upload"
      && upReq.value(forHTTPHeaderField: "Authorization") == "Bearer tok"
      && upReq.value(forHTTPHeaderField: "Content-Type") == "image/jpeg" && upReq.httpBody == Data([1, 2, 3]),
      "upload request: raw bytes with the token")
check(upReq.value(forHTTPHeaderField: "X-Filename") == "Beach%20day%2F%C3%BC.jpg", "the file name is percent-encoded")
let attTurnBody = { (r: URLRequest) in (try? JSONSerialization.jsonObject(with: r.httpBody ?? Data())) as? [String: Any] ?? [:] }
check((attTurnBody(ChatRequests.turn(upEP, text: "x", clientID: "c", attachments: [upID]))["attachments"] as? [String])
      == [upID], "a turn request carries the ids")
check(attTurnBody(ChatRequests.turn(upEP, text: "x", clientID: "c"))["attachments"] == nil,
      "a turn without attachments has no attachments key")
check(ChatUploaded.decode(Data(#"{"ok":true,"id":"\#(upID)","kind":"pdf","name":"lease.pdf"}"#.utf8))
      == ChatUploaded(id: upID, kind: "pdf", name: "lease.pdf"), "the daemon's upload answer")
check(ChatUploaded.decode(Data(#"{"ok":true,"id":"../x","kind":"pdf","name":"a"}"#.utf8)) == nil
      && ChatUploaded.decode(Data(#"{"ok":true,"id":"\#(upID)","kind":"exe","name":"a"}"#.utf8)) == nil
      && ChatUploaded.decode(Data(#"{"ok":false,"error":"too large"}"#.utf8)) == nil, "a bad upload answer is refused")
let pageJSON = ChatAttachments.pageJSON([ChatUploaded(id: upID, kind: "image", name: "a\"b.jpg", thumb: "data:image/jpeg;base64,AA==")])
let pageBack = (try? JSONSerialization.jsonObject(with: Data(pageJSON.utf8))) as? [[String: String]]
check(pageBack?.first?["name"] == "a\"b.jpg" && pageBack?.first?["thumb"] == "data:image/jpeg;base64,AA==",
      "the page's attachments JSON round-trips")
check(ChatFailure.from(status: 415, body: Data(#"{"ok":false,"error":"eve takes photos"}"#.utf8)).message
      == "eve takes photos", "a refused type says why")
print("ATTACHMENTS_OK")

// Opening an attachment full size (docs/app-chat.md, "eve's pictures and files"): the page's ask, the request,
// the local name.
check(ChatBridgeMessage(body: ["type": "openAttachment", "id": upID, "name": "desk.jpg", "kind": "image"])
      == .openAttachment(id: upID, name: "desk.jpg", kind: "image"), "openAttachment")
check(ChatBridgeMessage(body: ["type": "openAttachment", "id": upID, "kind": "file"])
      == .openAttachment(id: upID, name: "", kind: "file"), "openAttachment without a name; eve's file kind")
for bad: [String: Any] in [["id": "../../etc/passwd", "kind": "pdf"], ["id": upID.uppercased(), "kind": "pdf"],
                           ["id": upID, "kind": "exe"], ["id": upID], ["kind": "pdf"]] {
    check(ChatBridgeMessage(body: bad.merging(["type": "openAttachment"]) { a, _ in a }) == nil,
          "openAttachment refused: \(bad)")
}
let longName = String(repeating: "n", count: 500)
if case .openAttachment(_, let name, _)? = ChatBridgeMessage(body: ["type": "openAttachment", "id": upID, "name": longName,
                                                                   "kind": "doc"]) {
    check(name.count == ChatFiles.maxName, "a long name is cut")
} else {
    check(false, "a long name still opens")
}
let fileReq = ChatRequests.file(upEP, id: upID)
check(fileReq?.httpMethod == "GET" && fileReq?.url?.absoluteString == "https://x.trycloudflare.com/api/link/file/\(upID)"
      && fileReq?.value(forHTTPHeaderField: "Authorization") == "Bearer tok" && fileReq?.httpBody == nil,
      "the file request: GET with the token")
check(ChatRequests.file(upEP, id: "../x") == nil && ChatRequests.file(upEP, id: upID + "/x") == nil,
      "no file request for a malformed id")
check(ChatFiles.safeName("../../etc/passwd", kind: "text", mime: "text/plain") == "etcpasswd.txt",
      "no slash and no leading dot: one component")
check(!ChatFiles.safeName("../../etc/passwd", kind: "text", mime: "").contains("/"), "never a slash")
check(ChatFiles.safeName("", kind: "image", mime: "image/jpeg") == "Photo.jpg"
      && ChatFiles.safeName("  ", kind: "file", mime: "application/octet-stream") == "File",
      "an empty name is Photo or File, with the type's extension when known")
check(ChatFiles.safeName("lease", kind: "pdf", mime: "application/pdf; charset=binary") == "lease.pdf"
      && ChatFiles.safeName("photos.zip", kind: "file", mime: "application/zip") == "photos.zip",
      "an extension is added only when missing")
check(ChatFiles.safeName("a\u{0}b\u{202E}c.txt", kind: "text", mime: "text/plain") == "abc.txt",
      "control and format characters dropped")
let longSafe = ChatFiles.safeName(String(repeating: "x", count: 300) + ".pdf", kind: "pdf", mime: "application/pdf")
check(longSafe.count == 120 && longSafe.hasSuffix(".pdf"), "a long name keeps its extension")
check(ChatDownload.gone.message.contains("gone") && !ChatDownload.gone.addressMayHaveMoved, "a 404 says it is gone")
print("OPEN_ATTACHMENT_OK")
// eve's controls in the Work view (Shared/ControlsClient.swift): which ones, the requests, each card's answer.
let ctlEP = ChatEndpoint(base: URL(string: "https://x.trycloudflare.com")!, token: "tok")
let ctlInfo = ControlsInfo.decode(Data(#"{"lights":[{"name":"desk lamp","room":"office"},{"name":"strip"},{"name":""}],"spotify":true,"firetv":false}"#.utf8))
check(ctlInfo == ControlsInfo(lights: [LightName(name: "desk lamp", room: "office"), LightName(name: "strip", room: nil)],
                              spotify: true, firetv: false), "the controls listing, a nameless light left out")
check(ctlInfo?.available == [.spotify, .lights], "only what the Mac has, Music first")
check(ControlsInfo.decode(Data(#"{"lights":[],"spotify":false,"firetv":true}"#.utf8))?.available == [.firetv], "TV only")
check(ControlsInfo.decode(Data(#"{"lights":[],"spotify":false,"firetv":false,"memory":true}"#.utf8))?.available == [.memory], "memory only")
// Memory is in Settings, never an Apps row; Notes is the first Apps row (owner, 2026-10-10).
let withAll = ControlsInfo.decode(Data(#"{"lights":[{"name":"strip"}],"spotify":true,"firetv":true,"memory":true,"notes":true}"#.utf8))
check(withAll?.available == [.notes, .spotify, .lights, .firetv, .memory], "the Mac has all five")
check(withAll?.apps == [.notes, .spotify, .lights, .firetv], "Apps lists Notes first and never Memory")
check(ControlsInfo.decode(Data(#"{"lights":[],"spotify":false,"firetv":false,"memory":true}"#.utf8))?.apps == [], "memory alone is no Apps row")
check(ControlsInfo.decode(Data(#"{"lights":[],"spotify":false,"firetv":false}"#.utf8))?.notes == false, "an older Mac has no Notes")
// Pika, the car (Shared/PikaDrive.swift, pika.py): an Apps row only when the Mac says so, after the TV remote.
let withPika = ControlsInfo.decode(Data(#"{"lights":[],"spotify":true,"firetv":true,"memory":true,"notes":true,"pika":true}"#.utf8))
check(withPika?.apps == [.notes, .spotify, .firetv, .pika], "Pika is listed when the Mac has it, Memory never")
check(withAll?.pika == false && !(withAll?.apps.contains(.pika) ?? true), "no pika key: no Pika row (an older Mac, or off)")
check(ControlKind.pika.title == "Pika" && ControlKind.pika.rawValue == "pika", "the control's name is the daemon's")
// Hold to drive: a press drives with its own id, the beat repeats it, only the driving key's release stops.
var pikaHold = PikaHold()
check(pikaHold.renew() == nil && !pikaHold.holding, "nothing held: no beat")
check(pikaHold.press(.forward, id: "a") == PikaCommand(action: "drive", dir: .forward, hold: "a"), "a press drives")
check(pikaHold.renew() == PikaCommand(action: "drive", dir: .forward, hold: "a"), "the beat repeats the same press")
check(pikaHold.press(.left, id: "b") == PikaCommand(action: "drive", dir: .left, hold: "b"), "a second key takes over")
check(pikaHold.release(.forward) == nil && pikaHold.direction == .left, "lifting the replaced key says nothing")
check(pikaHold.release(.left) == PikaCommand(action: "stop", dir: nil, hold: "b") && pikaHold.renew() == nil,
      "lifting the driving key stops, and the beat ends")
check(pikaHold.release(.left) == nil, "a second lift sends nothing more")
_ = pikaHold.press(.back, id: "c")
pikaHold.ended("old")
check(pikaHold.holding, "the Mac ending an older press leaves this one")
pikaHold.ended("c")
check(!pikaHold.holding && pikaHold.renew() == nil, "the Mac ending this press stops the beat")
check(pikaHold.stop() == PikaCommand(action: "stop", dir: nil, hold: "c"), "Stop always sends a stop")
let pikaBody = PikaCommand(action: "drive", dir: .right, hold: "h").body
check(pikaBody["action"] as? String == "drive" && pikaBody["dir"] as? String == "right" && pikaBody["hold"] as? String == "h"
      && PikaCommand(action: "close").body.count == 1, "the bodies the daemon reads")
check(PikaHold.beat < .milliseconds(450 / 2), "two beats fit in the Mac's heartbeat window")
check(PikaReply.decode(Data(#"{"ok":false,"stopped":true,"line":"Pika is busy."}"#.utf8))
      == PikaReply(ok: false, stopped: true, line: "Pika is busy.") && PikaReply.decode(Data("[]".utf8)) == nil,
      "the panel's answer")
let pikaDrive = ControlsRequests.pika(ctlEP, PikaCommand(action: "drive", dir: .forward, hold: "h"))
check(pikaDrive.url?.absoluteString == "https://x.trycloudflare.com/api/link/controls/pika" && pikaDrive.httpMethod == "POST"
      && pikaDrive.value(forHTTPHeaderField: "Authorization") == "Bearer tok" && pikaDrive.timeoutInterval <= 3,
      "a beat is a short POST with the token")
let pikaVideo = ControlsRequests.pikaStream(ctlEP)
check(pikaVideo.url?.path == "/api/link/controls/pika/stream" && pikaVideo.httpMethod == "GET"
      && pikaVideo.value(forHTTPHeaderField: "Authorization") == "Bearer tok"
      && !(pikaVideo.url?.absoluteString.contains("tok") ?? true), "the video asks with the token in the header only")
// The OBSBOT webcam (Shared/ObsbotControl.swift, obsbot_panel.py): an Apps row only when the Mac says so, after Pika.
let withCam = ControlsInfo.decode(Data(#"{"lights":[],"spotify":false,"firetv":true,"memory":true,"notes":false,"pika":true,"obsbot":true}"#.utf8))
check(withCam?.apps == [.firetv, .pika, .obsbot], "the camera is listed when the Mac has it, after Pika")
check(withPika?.obsbot == false && !(withPika?.apps.contains(.obsbot) ?? true), "no obsbot key: no Camera row")
check(ControlKind.obsbot.title == "Camera" && ControlKind.obsbot.rawValue == "obsbot", "the control's name is the daemon's")
var camHold = ObsbotHold()
check(camHold.renew() == nil, "nothing held: no beat")
check(camHold.press(.left, id: "a") == ObsbotCommand(action: "hold", dir: .left, hold: "a"), "a press turns")
check(camHold.renew() == ObsbotCommand(action: "hold", dir: .left, hold: "a"), "the beat repeats the press")
check(camHold.press(.up, id: "b").dir == .up && camHold.release(.left) == nil, "lifting a replaced arrow says nothing")
check(camHold.release(.up) == ObsbotCommand(action: "stop", hold: "b") && camHold.renew() == nil, "lifting stops")
_ = camHold.press(.down, id: "c")
camHold.ended("c")
check(!camHold.holding, "the Mac ending the press stops the beat")
check(ObsbotHold.beat < .milliseconds(450 / 2), "two beats fit in the Mac's heartbeat window")
let camBody = ObsbotCommand(action: "zoom", level: 40).body
check(camBody["action"] as? String == "zoom" && camBody["level"] as? Int == 40 && camBody.count == 2, "zoom's body")
let trackBody = ObsbotCommand(action: "track", on: false).body
check(trackBody["on"] as? Bool == false && ObsbotCommand(action: "hold", dir: .right, hold: "h").body["dir"] as? String == "right",
      "the bodies the daemon reads")
let camStatus = ObsbotReply.decode(Data(#"{"ok":true,"line":"Tiny 3 awake.","camera":"Tiny 3","awake":true,"pan":12,"pitch":-4,"zoom":30,"presets":[{"n":1,"set":true,"name":"Desk"},{"n":2,"set":false,"name":""},{"n":3,"set":true,"name":""}],"modes":["single","desk"]}"#.utf8))
check(camStatus?.pan == 12 && camStatus?.pitch == -4 && camStatus?.zoom == 30 && camStatus?.awake == true
      && camStatus?.presets == [ObsbotPreset(n: 1, name: "Desk"), ObsbotPreset(n: 3, name: "Preset 3")]
      && camStatus?.modes == ["single", "desk"], "a status read: where it points and only the saved presets")
check(ObsbotReply.decode(Data(#"{"ok":false,"stopped":true,"line":""}"#.utf8))?.stopped == true
      && ObsbotReply.decode(Data("[]".utf8)) == nil, "a refused beat, and a body that isn't an object")
check(ObsbotModes.title("voice") == "Follow the voice" && ObsbotModes.title("new") == "New", "the modes' names")
let camBeat = ControlsRequests.obsbot(ctlEP, ObsbotCommand(action: "hold", dir: .up, hold: "h"))
check(camBeat.url?.absoluteString == "https://x.trycloudflare.com/api/link/controls/obsbot" && camBeat.httpMethod == "POST"
      && camBeat.value(forHTTPHeaderField: "Authorization") == "Bearer tok" && camBeat.timeoutInterval <= 3
      && ControlsRequests.obsbot(ctlEP, ObsbotCommand(action: "wake")).timeoutInterval >= 10,
      "a beat is a short POST with the token; a wake may wait for the camera")
let camVideo = ControlsRequests.obsbotStream(ctlEP)
check(camVideo.url?.path == "/api/link/controls/obsbot/stream" && camVideo.httpMethod == "GET"
      && !(camVideo.url?.absoluteString.contains("tok") ?? true), "the picture asks with the token in the header only")
// The video: whole JPEGs between the markers, across reads, only the newest kept.
func fakeJPEG(_ n: UInt8) -> Data { Data([0xFF, 0xD8, n, n, n, 0xFF, 0xD9]) }
var splitter = MJPEGSplitter()
let part = Data("--frame\r\nContent-Type: image/jpeg\r\n\r\n".utf8)
check(splitter.feed(part + fakeJPEG(1).prefix(4)) == nil, "half a frame: nothing yet")
check(splitter.feed(fakeJPEG(1).dropFirst(4) + part) == fakeJPEG(1), "the rest arrives: the frame")
check(splitter.feed(fakeJPEG(2) + part + fakeJPEG(3) + part + fakeJPEG(4).prefix(3)) == fakeJPEG(3), "two at once: the newest")
check(splitter.feed(Data([0xFF, 0xD9])) == Data([0xFF, 0xD8, 4, 0xFF, 0xD9]), "the frame left over ends on the next read")
check(splitter.feed(Data(repeating: 9, count: 100)) == nil && splitter.buffer.count <= 1, "bytes outside a frame are not kept")
var small = MJPEGSplitter(cap: 16)
_ = small.feed(Data([0xFF, 0xD8]) + Data(repeating: 7, count: 64))
check(small.buffer.isEmpty, "a frame that never ends is dropped at the cap")
// The stamps that measure latency (pika.video_part's X-Stamp, pika_rtc.frame_message's 8 bytes).
var stamped = MJPEGSplitter()
let stampedPart = Data("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: 7\r\nX-Stamp: 1760100000123\r\n\r\n".utf8)
check(stamped.feed(stampedPart + fakeJPEG(5)) == fakeJPEG(5) && stamped.stamp == 1760100000123, "a part's X-Stamp is read")
check(stamped.feed(part + fakeJPEG(6)) == fakeJPEG(6) && stamped.stamp == nil, "a part without one has none")
let rtcMessage = Data([0, 0, 0x01, 0x99, 0xCE, 0x22, 0xA1, 0x7B]) + fakeJPEG(7)     // 1760100000123
check(PikaFrameMessage.parse(rtcMessage).map { $0.stamp == 1760100000123 && $0.jpeg == fakeJPEG(7) } == true,
      "a WebRTC frame is its stamp, then the JPEG as the camera made it")
check(PikaFrameMessage.parse(Data([1, 2, 3])) == nil, "a short message is not a frame")
var lat = PikaLatency(path: "webrtc", every: 2)
var report: [String: Any]?
for i in 0..<20 { report = lat.add(stamp: Double(i) * 100, now: Double(i) * 100 + (i < 18 ? 40 : 300)) ?? report }
check(report?["path"] as? String == "webrtc" && report?["p50"] as? Double == 40 && report?["p95"] as? Double == 300
      && (report?["n"] as? Int ?? 0) >= 10, "latency: the median and the slow tail of one path")
// Notes (Shared/NotesText.swift, link_notes.py): the list, a page, its blocks and a to-do's tick.
let notesList = NotesReply.decode(Data(#"{"ok":true,"line":"","folders":["00-vision","01-inbox","03-projects","03-projects/garden","09-archive"],"notes":[{"path":"03-projects/garden/beds.md","folder":"03-projects/garden","title":"Beds","preview":"Raised","modified":1700000100},{"path":"01-inbox/tea.md","folder":"01-inbox","title":"Tea","preview":"buy oolong","modified":1700000200},{"path":"CLAUDE.md","folder":"","title":"This vault","preview":"","modified":1700000300},{"path":"01-inbox/old.md","folder":"01-inbox","title":"Old","preview":"","modified":1600000000},{"path":"08-journals/daily/2026-10-10.md","folder":"08-journals/daily","title":"Today","preview":"","modified":1700000000},{"path":"","title":"no path"}]}"#.utf8))
check(notesList?.ok == true && notesList?.notes.count == 5, "the list decodes; a note without a path is left out")
let grouped = NotesText.group(notesList?.notes ?? [], order: notesList?.folders ?? [])
check(grouped.map(\.path) == ["01-inbox", "03-projects/garden", "08-journals/daily", ""], "folders in the vault's order, root last")
check(grouped.first?.notes.map(\.title) == ["Tea", "Old"], "newest first in a folder")
check(grouped.map(\.name) == ["Inbox", "Projects · Garden", "Journals · Daily", "Vault"], "folder names read as words")
check(NotesText.folderSymbol("01-inbox") == "tray" && NotesText.folderSymbol("x") == "folder", "folder symbols")
check(NotesText.filter(notesList?.notes ?? [], "oolong").map(\.path) == ["01-inbox/tea.md"], "the search field filters as you type")
check(NotesText.filter(notesList?.notes ?? [], "garden beds").map(\.path) == ["03-projects/garden/beds.md"], "every word, the folder too")
let notePage = NotesReply.decode(Data(##"{"ok":true,"path":"01-inbox/tea.md","title":"Tea","body":"# Tea\n","front":["created: 2026-10-01",""],"revision":"abc","editable":false,"undo_id":"","line":""}"##.utf8))
check(notePage?.front == ["created: 2026-10-01"] && notePage?.revision == "abc" && notePage?.editable == false
      && notePage?.undoID == nil, "a page decodes; an empty undo id is none")
let gone = NotesReply.decode(Data(#"{"ok":true,"path":"01-inbox/tea.md","trash_id":"t1","line":"Deleted."}"#.utf8))
check(gone?.trashID == "t1" && gone?.line == "Deleted.", "a delete's restore id")
let pageBody = "# Tea\n\nSome *tea* with [[Oolong|the good one]].\n- [ ] buy oolong\n  - [x] boil water\n- milk\n1. first\n> said once\n```\nlet x = 1\n```\n---\n## Next C#\n"
let blocks = NotesText.blocks(pageBody)
check(blocks.map(\.kind) == [.heading(1), .text, .todo(done: false), .todo(done: true), .bullet, .numbered("1."), .quote, .code, .rule, .heading(2)],
      "every kind of block, blank lines left out")
check(blocks[2].line == 3 && blocks[3].indent == 1 && blocks[3].text == "boil water", "a to-do's line and nesting")
check(blocks[7].text == "let x = 1" && blocks[9].text == "Next C#", "code kept whole; a heading keeps its own #")
check(NotesText.inline(blocks[1].text) == "Some *tea* with the good one.", "a wiki link reads as its alias")
check(NotesText.inline("see [[Beds]] and [[x|y]]") == "see Beds and y", "wiki links read as words")
let ticked = NotesText.toggle(pageBody, line: 3)
check(ticked?.contains("- [x] buy oolong") == true && ticked?.contains("  - [x] boil water") == true, "a tick checks one line")
check(NotesText.toggle(ticked ?? "", line: 4)?.contains("  - [ ] boil water") == true, "and unchecks a nested one")
check(NotesText.toggle(pageBody, line: 0) == nil && NotesText.toggle(pageBody, line: 99) == nil, "only a to-do toggles")
check(NotesText.toggle("- [ ] a\r\nb", line: 0) == "- [x] a\r\nb", "a CRLF line keeps its CR")
var utc = Calendar(identifier: .gregorian)
utc.timeZone = TimeZone(identifier: "UTC")!
let stampNow = Date(timeIntervalSince1970: 1_760_000_000)           // 2025-10-09, a Thursday
check(NotesText.stamp(stampNow.addingTimeInterval(-86_400), now: stampNow, calendar: utc) == "Yesterday", "yesterday")
check(!NotesText.stamp(stampNow.addingTimeInterval(-86_400 * 40), now: stampNow, calendar: utc).isEmpty, "an older day")
print("NOTES_OK")
let memReply = MemoryReply.decode(Data(#"{"ok":true,"line":"","hits":[{"id":"ana","line":"- likes tea","src":"2026-10-01 09:00"},{"id":"x","line":""}],"recalled":["(2026-10-01, app) likes tea",""],"said":[{"day":"2026-10-01","time":"09:00","ch":"telegram","who":"owner","text":"i like tea"}]}"#.utf8))
check(memReply?.hits == [MemoryFact(id: "ana", line: "- likes tea", src: "2026-10-01 09:00")], "a fact decodes; an empty line is dropped")
check(memReply?.recalled == ["(2026-10-01, app) likes tea"] && memReply?.said.first?.who == "owner", "recalled and said decode")
let memPreview = MemoryReply.decode(Data(#"{"ok":true,"total":3,"token":"tok1","line":"3 things match"}"#.utf8))
check(memPreview?.total == 3 && memPreview?.token == "tok1" && memPreview?.isEmpty == true, "a forget preview decodes")
check(MemoryReply.decode(Data(#"{"ok":false,"line":"off","token":""}"#.utf8))?.token == nil, "an empty token is none")
let memHidden = MemoryReply.decode(Data(#"{"ok":true,"hidden":["the dentist","","likes tea"],"line":""}"#.utf8))
check(memHidden?.hidden == ["the dentist", "likes tea"] && memHidden?.isEmpty == true, "the hidden list decodes; an empty one is dropped")
check(memReply?.hidden == [], "no hidden key is an empty list")
check(MemoryReply.decode(Data(#"{"ok":true,"line":"eve won't bring that up again."}"#.utf8))?.line == "eve won't bring that up again.", "a hide's answer decodes")

// The profile's markdown as rows (Shared/MemoryText.swift): front matter and the title go, sections and facts stay.
let profileMD = """
---
id: profile
updated: 2026-10-08
---

# What eve knows about its owner

Dreamt from conversations up to 2026-10-08.

## Life context
- They co-founded [[project-storeybox]], a booth ([[project-storeybox]]) (said 2026-09-28).
- A fact that wraps
  onto a second line (said 2026-09-22).

## Preferences
- Likes tea.
"""
let prof = MemoryText.profile(profileMD)
check(prof.updated == "2026-10-08" && prof.intro == "Dreamt from conversations up to 2026-10-08", "front matter and intro")
check(prof.sections.map(\.title) == ["Life context", "Preferences"], "sections by ## heading")
check(prof.sections[0].items[0] == MemoryItem(text: "They co-founded storeybox, a booth", said: "2026-09-28"), "wikilinks as words, a trailing record name dropped, the said tail as the day")
check(MemoryText.clean("uses [[project-claude-mem]] daily ([[project-x-y]])") == "uses claude-mem daily", "an inline name keeps its hyphens; a trailing one goes")
check(MemoryText.clean("a (real aside)") == "a (real aside)", "words in parentheses stay")
check(prof.sections[0].items[1] == MemoryItem(text: "A fact that wraps onto a second line", said: "2026-09-22"), "a wrapped fact is one row")
check(prof.sections[1].items == [MemoryItem(text: "Likes tea", said: nil)], "no tail, no day")
check(MemoryText.item("Routes searches by Jev ([[preference-web-search-routing]]) (owner, 2026-09-29).") == MemoryItem(text: "Routes searches by Jev", said: "2026-09-29"), "an (owner, day) tail and a trailing link")
check(MemoryText.profile("").isEmpty && MemoryText.profile("# Title only\n").isEmpty, "an empty profile is empty")
check(MemoryText.recalled("(2026-10-01, app) likes tea") == (MemoryText.dayLabel("2026-10-01") + " · app", "likes tea"), "a recalled line splits into caption and text")
check(MemoryText.recalled("plain words") == ("", "plain words"), "no head, no caption")
check(MemoryText.dayLabel("2025-09-22") == "Sep 22, 2025" && MemoryText.dayLabel("junk") == "junk", "a day label")
check(MemoryText.whoLabel("owner") == "you" && MemoryText.whoLabel("eve") == "eve", "who")
check(ControlsInfo.decode(Data("[]".utf8)) == nil, "a listing that is not an object is refused")
let ctlList = ControlsRequests.list(ctlEP)
check(ctlList.httpMethod == "GET" && ctlList.url?.path == "/api/link/controls"
      && ctlList.value(forHTTPHeaderField: "Authorization") == "Bearer tok", "controls listing request")
let ctlPress = ControlsRequests.press(ctlEP, .firetv, body: TvPress(action: "up", count: 3).body)
let ctlPressBody = (try? JSONSerialization.jsonObject(with: ctlPress.httpBody ?? Data())) as? [String: Any] ?? [:]
check(ctlPress.httpMethod == "POST" && ctlPress.url?.path == "/api/link/controls/firetv"
      && ctlPress.value(forHTTPHeaderField: "Authorization") == "Bearer tok"
      && ctlPressBody["action"] as? String == "up" && ctlPressBody["repeat"] as? Int == 3, "a remote press request")
// The registry's apps (app_registry.py, 2026-10-12): the Mac's "apps" list in its order, native or generic sheets.
// An older Mac (no "apps"): today's rows from the legacy keys, unchanged.
check(withAll?.entries == nil && withAll?.rows == [.native(.notes), .native(.spotify), .native(.lights), .native(.firetv)],
      "no apps key: the legacy rows, Notes first, Memory never")
check(withCam?.rows == [.native(.firetv), .native(.pika), .native(.obsbot)], "no apps key: Pika and Camera from the legacy keys")
let regJSON = #"""
{"lights":[{"name":"desk lamp","room":"office"}],"spotify":true,"firetv":false,"memory":true,"notes":true,"pika":false,
 "obsbot":true,
 "apps":[{"id":"spotify","title":"Music","symbol":"music.note","detail":"Spotify","sheet":"native"},
         {"id":"lights","title":"Lights","symbol":"lightbulb","detail":"1 light","sheet":"native"},
         {"id":"obsbot","title":"Camera","symbol":"web.camera","detail":"OBSBOT webcam","sheet":"native"},
         {"id":"fan","title":"Fan","symbol":"fan","detail":"Desk fan","sheet":"generic"},
         {"id":"memory","title":"Memory","sheet":"native"},{"id":"notes","title":"Notes","sheet":"native"},
         {"id":"Bad/Id","title":"x"},{"title":"no id"},{"id":"fan","title":"Fan again"},
         {"id":"kettle"},{"id":"heater","title":"Heater","sheet":"native"},{"id":"car9","sheet":"sideways"}]}
"""#
let reg = ControlsInfo.decode(Data(regJSON.utf8))
let fan = AppEntry(id: "fan", title: "Fan", symbol: "fan", detail: "Desk fan", sheet: "generic")
check(reg?.rows.map(\.id) == ["notes", "spotify", "lights", "obsbot", "fan", "kettle", "heater", "car9"],
      "the Mac's order after Notes; Memory, Notes, bad ids and repeats are no rows")
check(reg?.rows.prefix(4) == [.native(.notes), .native(.spotify), .native(.lights), .native(.obsbot)],
      "the owner's apps open their native sheets")
check(reg?.rows[4] == .generic(fan), "an app with no Swift opens the generic sheet")
check(reg?.rows[5] == .generic(AppEntry(id: "kettle", title: "Kettle")), "a bare entry: its id as the title, the fallback symbol")
if case .generic(let heater)? = reg?.rows[6] { check(heater.title == "Heater", "native asked for, but the phone has no sheet: generic") }
else { check(false, "native asked for, but the phone has no sheet: generic") }
if case .generic(let car)? = reg?.rows[7] { check(car.sheet == "sideways" && car.title == "Car9", "an unknown sheet kind: generic") }
else { check(false, "an unknown sheet kind: generic") }
let tvGeneric = ControlsInfo.decode(Data(#"{"lights":[],"spotify":false,"firetv":true,"apps":[{"id":"firetv","title":"TV","sheet":"generic"}]}"#.utf8))
check(tvGeneric?.rows == [.generic(AppEntry(id: "firetv", title: "TV", detail: "", sheet: "generic"))],
      "the Mac's sheet decides: a known app it marks generic opens the generic sheet")
check(reg?.entry(for: .lights)?.detail == "1 light" && reg?.entry(for: .firetv) == nil, "a native row reads the Mac's words")
let noNotes = ControlsInfo.decode(Data(#"{"lights":[],"spotify":false,"firetv":false,"notes":false,"apps":[{"id":"fan"}]}"#.utf8))
check(noNotes?.rows == [.generic(AppEntry(id: "fan", title: "Fan"))], "no Notes on this Mac: only the apps")
check(ControlsInfo.decode(Data(#"{"lights":[],"spotify":true,"firetv":false,"apps":[]}"#.utf8))?.rows == [],
      "an empty apps list is the Mac's word: no rows, not the legacy ones")
check(ControlsInfo.decode(Data(#"{"lights":[],"spotify":true,"firetv":false,"apps":"junk"}"#.utf8))?.rows == [.native(.spotify)],
      "an apps value that is not a list: the legacy rows")
check(AppEntry.validID("fan") && AppEntry.validID("a") && AppEntry.validID("cam2") && !AppEntry.validID("2cam")
      && !AppEntry.validID("Fan") && !AppEntry.validID("a-b") && !AppEntry.validID("") && !AppEntry.validID(String(repeating: "a", count: 17)),
      "the registry's slot rule")
let longTitle = AppEntry(json: ["id": "fan", "title": String(repeating: "x", count: 100)])
check(longTitle?.title.count == AppEntry.maxTitle && longTitle?.title.hasSuffix("…") == true, "a long title is trimmed")
// The generic sheet's panel (docs/adding-an-app.md): the same example the bridge's check_panel validates.
let panelInline = #"""
{"ok": true, "ui": 1,
 "line": "The fan is on.",
 "status": "On · speed 3",
 "controls": [
   {"type": "toggle", "id": "power", "label": "Power", "on": true, "action": "power"},
   {"type": "slider", "id": "speed", "label": "Speed", "value": 3, "min": 1, "max": 5, "step": 1, "action": "speed"},
   {"type": "button", "id": "boost", "label": "Boost", "symbol": "bolt", "action": "boost"},
   {"type": "button", "id": "off",   "label": "Turn off", "action": "off", "role": "destructive"}
 ]}
"""#
let panelFixture = (try? Data(contentsOf: URL(fileURLWithPath: "bridge/tests/fixtures/app_panel_example.json")))
let panelExample = AppPanel.decode(panelFixture ?? Data(panelInline.utf8))
print(panelFixture == nil ? "app panel: the inline example (no bridge fixture yet)" : "app panel: the bridge's fixture")
let powerCtl = AppControl(id: "power", label: "Power", action: "power", kind: .toggle(on: true))
let speedCtl = AppControl(id: "speed", label: "Speed", action: "speed", kind: .slider(value: 3, min: 1, max: 5, step: 1))
let boostCtl = AppControl(id: "boost", label: "Boost", action: "boost", kind: .button(symbol: "bolt", destructive: false))
let offCtl = AppControl(id: "off", label: "Turn off", action: "off", kind: .button(symbol: nil, destructive: true))
check(panelExample == AppPanel(ok: true, ui: 1, line: "The fan is on.", status: "On · speed 3",
                               controls: [powerCtl, speedCtl, boostCtl, offCtl]), "the contract's example decodes")
let oddPanel = AppPanel.decode(Data(#"""
{"ok":true,"ui":2,"controls":[{"type":"dial","id":"d","action":"spin"},{"type":"slider","id":"s","action":"s","min":5,"max":5},
 {"type":"slider","id":"t","action":"t","min":9,"max":1},{"type":"button","id":"nolabel","action":"go"},
 {"type":"button","label":"No action"},{"type":"toggle","id":"nolabel","action":"again"},
 {"type":"slider","id":"w","action":"w","min":0,"max":10,"value":42,"step":0},
 {"type":"slider","id":"b","action":"b","min":true,"max":10}]}
"""#.utf8))
check(oddPanel?.controls?.map(\.id) == ["nolabel", "w"],
      "unknown types, empty sliders, no action, a repeated id and a boolean bound are dropped")
check(oddPanel?.controls?.first?.label == "go" && oddPanel?.ui == 2, "a control with no label shows its action")
check(oddPanel?.controls?.last?.kind == .slider(value: 10, min: 0, max: 10, step: nil), "a value past the end is held, a zero step dropped")
let manyJSON = "{\"ok\":true,\"controls\":[" + (0..<30).map { #"{"type":"button","id":"b\#($0)","action":"a\#($0)"}"# }
    .joined(separator: ",") + "]}"
check(AppPanel.decode(Data(manyJSON.utf8))?.controls?.count == AppPanel.maxControls, "at most 24 controls")
check(AppPanel.decode(Data(#"{"ok":true,"line":"Boosted."}"#.utf8))?.controls == nil, "an answer with no controls says so")
check(AppPanel.decode(Data(#"{"ok":false,"reason":"The fan is unplugged."}"#.utf8))
      == AppPanel(ok: false, ui: 1, line: "The fan is unplugged.", status: nil, controls: nil), "a refusal's line")
check(AppPanel.decode(Data("[]".utf8)) == nil, "an answer that is not an object is refused")
// What each control sends back.
check(boostCtl.body() as NSDictionary == ["action": "boost"] as NSDictionary, "a button sends its action")
check(powerCtl.body(on: false) as NSDictionary == ["action": "power", "on": false] as NSDictionary, "a toggle sends its new setting")
check(speedCtl.body(value: 4.4) as NSDictionary == ["action": "speed", "value": 4] as NSDictionary
      && speedCtl.body(value: 4.4)["value"] is Int, "a slider sends its value on its step, whole as a whole number")
check(speedCtl.body(value: 99)["value"] as? Int == 5 && speedCtl.body(value: -3)["value"] as? Int == 1, "held to the range")
let fineCtl = AppControl(id: "f", label: "F", action: "f", kind: .slider(value: 0, min: 0, max: 1, step: nil))
check(fineCtl.body(value: 0.25)["value"] as? Double == 0.25, "a slider with no step sends what was set")
check(AppControl.snap(7, min: 1, max: 10, step: 3) == 7 && AppControl.snap(8.6, min: 1, max: 10, step: 3) == 10
      && AppControl.snap(.nan, min: 1, max: 10, step: nil) == 1, "steps count from min")
// The sheet's model: one press at a time, shown at once, taken back on a failure; an answer without controls keeps them.
var appModel = AppPanelModel()
check(!appModel.answered && appModel.controls.isEmpty, "a new sheet waits for its first answer")
check(appModel.refresh() as NSDictionary? == ["action": "status"] as NSDictionary, "the sheet opens with a status read")
check(appModel.refresh() == nil && appModel.busy, "a second call waits for the first")
appModel.finish(.success(panelExample!))
check(appModel.answered && !appModel.busy && appModel.status == "On · speed 3" && appModel.line == "The fan is on."
      && appModel.controls.count == 4 && !appModel.failed, "the first answer draws the sheet")
check(appModel.press(powerCtl, on: false) as NSDictionary? == ["action": "power", "on": false] as NSDictionary,
      "a toggle press")
check(appModel.controls.first?.kind == .toggle(on: false) && appModel.press(boostCtl) == nil, "shown at once; one press at a time")
appModel.finish(.success(AppPanel(ok: true, ui: 1, line: "Off.", status: nil, controls: nil)))
check(appModel.controls.first?.kind == .toggle(on: false) && appModel.line == "Off." && appModel.status == "On · speed 3",
      "an answer with no controls or status keeps the last ones")
_ = appModel.press(speedCtl, value: 5)
appModel.finish(.failure(ChatFailure(message: "eve didn't answer.", addressMayHaveMoved: false)))
check(appModel.controls[1].kind == .slider(value: 3, min: 1, max: 5, step: 1) && appModel.failed
      && appModel.line == "eve didn't answer.", "a failed press takes its setting back and says why")
_ = appModel.press(powerCtl, on: true)
appModel.finish(.success(AppPanel(ok: false, ui: 1, line: "", status: nil, controls: nil)))
check(appModel.controls.first?.kind == .toggle(on: false) && appModel.failed && appModel.line == "That didn't work.",
      "a refusal takes the setting back")
_ = appModel.press(offCtl)
appModel.finish(.success(AppPanel(ok: true, ui: 1, line: "Off.", status: "Off", controls: [powerCtl])))
check(appModel.controls == [powerCtl] && appModel.status == "Off" && !appModel.failed, "an answer with controls redraws them")
check(appModel.press(speedCtl, value: 2) == nil && !appModel.busy, "a control that is gone sends nothing")
// The request: the same door as the native sheets.
let appPress = ControlsRequests.pressApp(ctlEP, id: "fan", body: powerCtl.body(on: true))
let appPressBody = (try? JSONSerialization.jsonObject(with: appPress.httpBody ?? Data())) as? [String: Any] ?? [:]
check(appPress.httpMethod == "POST" && appPress.url?.path == "/api/link/controls/fan"
      && appPress.value(forHTTPHeaderField: "Authorization") == "Bearer tok" && appPress.timeoutInterval == 30
      && appPressBody["action"] as? String == "power" && appPressBody["on"] as? Bool == true, "a generic press request")
check(ControlsRequests.press(ctlEP, .lights, body: [:]).url == ControlsRequests.pressApp(ctlEP, id: "lights", body: [:]).url,
      "a native press is the same request")
print("APP_REGISTRY_OK")
check(TvPress(action: "select").body.count == 1, "one tap sends no repeat")
check(LightChoice.choices([LightName(name: "lamp", room: "office")]).map(\.value) == ["all"], "one light: All only")
check(LightChoice.choices([LightName(name: "lamp", room: "office"), LightName(name: "strip", room: "office"),
                           LightName(name: "bulb", room: nil)]).map(\.label) == ["All", "Office", "Lamp", "Strip", "Bulb"],
      "All, each room once, each light")
check(LightSwatch.hex(red: 1, green: 0.5, blue: -1) == "#ff8000", "a picked colour as hex, clamped")
check(LightSwatch.rgb("#ff8000").map { $0.0 == 1 && $0.2 == 0 } == true && LightSwatch.rgb("#zz") == nil, "hex back")
let ltStatus = LightsReply.decode(Data(##"{"ok":true,"lights":[{"name":"lamp","reachable":true,"on":true,"brightness":60,"color":"cyan","rgb":"#19ffff"},{"name":"strip","reachable":false,"reason":"timeout"},{"name":"bulb","reachable":true,"on":false}]}"##.utf8))
check(ltStatus?.said == "lamp: cyan 60% · strip: not answering · bulb: off" && ltStatus?.brightness == 60,
      "a lights status read, said as the Mini App says it")
check(LightsReply.decode(Data(#"{"ok":false,"line":"Unknown colour 'x'."}"#.utf8))?.said == "Unknown colour 'x'.",
      "a refused change says why")
let music = MusicState.decode(Data(#"{"ok":true,"playing":true,"track":"Song","artists":["A","B"],"album":"LP","art":"https://i.scdn.co/image/ab","device":{"name":"Mac","volume":40,"active":true},"devices":[{"name":"Mac","active":true},{"name":"Echo","restricted":true}],"mode":false,"line":"Playing."}"#.utf8))
check(music?.track == "Song" && music?.byLine == "A, B · LP" && music?.volume == 40 && music?.playing == true
      && music?.art?.absoluteString == "https://i.scdn.co/image/ab" && music?.devices.count == 2
      && music?.devices.last?.restricted == true && music?.mode == false && music?.line == "Playing.", "a Music answer")
check(MusicState.decode(Data(#"{"ok":true,"track":"x","art":"https://evil.example/a.jpg"}"#.utf8))?.art == nil,
      "artwork only from Spotify's image host")
check(MusicState.decode(Data(#"{"ok":false,"reason":"Couldn't reach Spotify."}"#.utf8))?.line == "Couldn't reach Spotify.",
      "a Music failure says why")
let tv = TvState.decode(Data(#"{"ok":true,"power":"screensaver","app":null,"volume":44,"muted":false,"now":"TV on — screensaver, volume 44%","line":"","apps":["YouTube","Netflix"]}"#.utf8))
check(tv?.isOn == true && tv?.volume == 44 && tv?.muted == false && tv?.apps == ["YouTube", "Netflix"]
      && tv?.now?.hasPrefix("TV on") == true, "a TV answer")
var tvq = TvQueue()
tvq.push(TvPress(action: "down"))
check(tvq.next() == TvPress(action: "down") && tvq.next() == nil, "one press at a time")
tvq.push(TvPress(action: "down")); tvq.push(TvPress(action: "down")); tvq.push(TvPress(action: "select"))
tvq.push(TvPress(action: "volume", volume: 20)); tvq.push(TvPress(action: "volume", volume: 35))
check(tvq.pending.map(\.action) == ["down", "down", "select", "volume"] && tvq.pending[1].count == 2
      && tvq.pending[3].volume == 35, "taps behind the one on its way coalesce; the newest volume wins")
tvq.finished()
check(tvq.next() == TvPress(action: "down", count: 2), "the coalesced press goes next, in order")
tvq.push(TvPress(action: "power_on")); tvq.push(TvPress(action: "power_on"))
check(tvq.pending.suffix(2).map(\.action) == ["power_on", "power_on"], "only d-pad keys coalesce")
check(WorkFailure.from(status: 404, body: Data(#"{"ok":false,"line":"eve has no TV on this computer."}"#.utf8),
                       for: .controls).message == "eve has no TV on this computer.", "a missing device says so")
check(WorkFailure.from(status: 404, body: Data("not found".utf8), for: .controls).message.contains("update"),
      "an older eve without controls says to update it")
print("CONTROLS_OK")

// The home-screen widget (Shared/Today.swift, docs/widget.md): GET /api/link/today, the cache, the fallback, the rows.
check(LinkRoute(URL(string: "evelink://work")!) == .work, "the widget's tap routes to the Work view")
let todayJSON = Data(#"""
{"at": 1000, "needs_you": 3, "needs": [{"id": "c1", "kind": "job", "title": "Book it", "line": "Waiting on your Go"},
 {"id": "c2", "kind": "job", "title": "", "line": "no title: dropped"}],
 "going": [{"id": "c5", "kind": "job", "title": "Compare flights", "line": "Researching"},
           {"id": "w1", "kind": "watch", "title": "Desk chair", "line": "$320.00"}],
 "today": [{"title": "Trip", "start": 0, "end": 99999, "all_day": true},
           {"title": "Review", "start": 900, "end": 1800, "all_day": false, "place": "Room 4"},
           {"title": "Breakfast", "start": 100, "end": 400, "all_day": false},
           {"title": "Dentist", "start": 4000, "end": 5000, "all_day": false}, {"start": 1}],
 "now": [{"kind": "music", "title": "A song", "line": "Someone"}], "sources": {"jobs": "ok"}}
"""#.utf8)
let todayFeed = TodayFeed.decode(todayJSON)
check(todayFeed?.needsYou == 3 && todayFeed?.needs.map(\.id) == ["c1"] && todayFeed?.going.count == 2
      && todayFeed?.events.count == 4 && todayFeed?.now.first?.kind == "music", "the feed decodes; untitled rows drop")
check(todayFeed?.events(at: Date(timeIntervalSince1970: 1000)).map(\.title) == ["Trip", "Review", "Dentist"],
      "an event that ended is not drawn")
check(TodayFeed.decode(Data("{}".utf8)) == nil && TodayFeed.decode(Data("[]".utf8)) == nil, "no 'at': refused")
let emptyFeed = TodayFeed.decode(Data(#"{"at": 5}"#.utf8))
check(emptyFeed?.isEmpty(at: Date()) == true && emptyFeed?.needsYou == 0, "an empty day is a feed, not an error")
if let f = todayFeed {
    let at = Date(timeIntervalSince1970: 1000)
    let secs = TodayLayout.sections(f, at: at, budget: 12)
    check(secs.map(\.kind) == [.needs, .going, .today, .now], "sections in the owner's order")
    check(secs.map(\.lines.count) == [1, 2, 3, 1] && secs[0].count == 3 && secs[0].more == 2,
          "everything that fits: 4 headings + 7 rows; needs counts all it has")
    let tight = TodayLayout.sections(f, at: at, budget: 6)
    check(tight.map(\.kind) == [.needs, .going, .today] && tight.map(\.lines.count) == [1, 1, 1],
          "too little room: needs, then today, then going keep a heading and a row; now waits")
    check(secs[2].lines.map(\.lead) == [.symbol("calendar"), .now, .time(Date(timeIntervalSince1970: 4000))],
          "all-day, under way, later")
    check(TodayLayout.flat(f, at: at, rows: 4).map(\.title) == ["Book it", "Trip", "Review", "Dentist"],
          "medium: the top four rows, needs first, then today")
    check(secs[1].lines.map(\.lead) == [.symbol("circle.dashed"), .symbol("eye")], "a job and a watch")
}

// The fallback: fresh data is kept; a dead tunnel asks the rendezvous once; otherwise the last good data, stamped.
final class FakeNet: @unchecked Sendable {
    var answers: [String: (Int, Data)] = [:]       // host -> status, body
    var asked: [String] = []
    var newAddress: URL?
    var rendezvousAsked = 0
    var repaired: [Pairing] = []
}
let net = FakeNet()
let cacheURL = FileManager.default.temporaryDirectory.appendingPathComponent("eve-today-\(UUID().uuidString).json")
let todayCache = TodayCache(url: cacheURL)
func loadToday(_ pairing: Pairing?, at: Date = Date(timeIntervalSince1970: 2000)) async -> TodaySnapshot {
    await TodayLoader.load(
        pairing: pairing, cache: todayCache, now: at,
        fetch: { r in
            let host = r.url?.host ?? ""
            net.asked.append(host)
            check(r.value(forHTTPHeaderField: "Authorization") == "Bearer " + String(repeating: "t", count: 32)
                  && r.url?.path == "/api/link/today" && r.httpMethod == "GET", "the widget's request")
            guard let a = net.answers[host] else { throw URLError(.cannotFindHost) }
            return (a.1, a.0)
        },
        rendezvous: { _ in net.rendezvousAsked += 1; return net.newAddress },
        repaired: { net.repaired.append($0) })
}
let paired = Pairing(base: URL(string: "https://old-name.trycloudflare.com")!, token: String(repeating: "t", count: 32))
var snap = await loadToday(nil)
check(snap == TodaySnapshot(feed: nil, fetched: nil, problem: .unpaired) && net.asked.isEmpty, "unpaired: no request")
snap = await loadToday(paired)
check(snap.problem == .unreachable && snap.feed == nil && net.rendezvousAsked == 1, "nothing yet, nobody answers")
net.answers["old-name.trycloudflare.com"] = (200, todayJSON)
snap = await loadToday(paired)
check(snap.problem == nil && snap.feed == todayFeed && !snap.stale && todayCache.load()?.0 == todayFeed,
      "a good answer is shown and kept")
net.answers["old-name.trycloudflare.com"] = (530, Data())
net.newAddress = URL(string: "https://new-name.trycloudflare.com")!
net.answers["new-name.trycloudflare.com"] = (200, todayJSON)
net.rendezvousAsked = 0
snap = await loadToday(paired)
check(snap.problem == nil && net.rendezvousAsked == 1 && net.repaired.map(\.base.host) == ["new-name.trycloudflare.com"]
      && net.asked.suffix(2) == ["old-name.trycloudflare.com", "new-name.trycloudflare.com"],
      "a gone tunnel: eve's new address from the rendezvous, kept, and read there")
net.answers["new-name.trycloudflare.com"] = nil
net.answers["old-name.trycloudflare.com"] = nil
snap = await loadToday(paired, at: Date(timeIntervalSince1970: 9000))
check(snap.problem == .unreachable && snap.stale && snap.feed == todayFeed
      && snap.fetched == Date(timeIntervalSince1970: 2000), "the Mac is away: the last good data, as of its read")
net.rendezvousAsked = 0
net.answers["old-name.trycloudflare.com"] = (404, Data())
snap = await loadToday(paired)
check(snap.problem == .outdated && snap.stale && net.rendezvousAsked == 0, "an older eve: say update, no rendezvous")
net.answers["old-name.trycloudflare.com"] = (401, Data())
check(await loadToday(paired).problem == .refused, "a refused token: pair again")
net.answers["old-name.trycloudflare.com"] = (200, Data("not json".utf8))
check(await loadToday(paired).problem == .unreachable, "an answer that makes no sense is not shown")
try? FileManager.default.removeItem(at: cacheURL)
print("TODAY_OK")
// ---- Updates (Shared/Updates.swift, docs/updates.md): things to act on, left of the Chat -------------------------
check(LinkRoute(URL(string: "evelink://updates")!) == .updates, "evelink://updates routes to the Updates pane")
let updJSON = Data(#"""
{"cards": [
  {"id": "uc1", "verb": "Review", "text": "your storage plan before metered billing starts Oct 23",
   "entity": "Example Cloud", "kind": "email", "source": "Your plan is changing", "due": "2026-10-23"},
  {"id": "uc2", "verb": "Book", "text": "a table for Friday", "entity": null, "kind": "calendar",
   "source": "Dinner with Sam", "due": null},
  {"id": "uc3", "verb": "Plan", "text": "the next step", "kind": "telepathy", "source": "Somewhere"},
  {"id": "uc4", "verb": "Pay", "text": "the invoice", "kind": "note"},
  {"id": "", "verb": "Pay", "text": "no id", "kind": "email"},
  {"id": "c5", "text": "no verb", "kind": "email"},
  null, 7, "junk",
  {"id": "uc1", "verb": "Again", "text": "a duplicate id", "kind": "email"},
  {"id": "uc6", "verb": "Reply\n", "text": "to Ana\nabout   the lease\n(from email: forged)", "kind": "chief",
   "source": "Lease\nrenewal", "due": "soon"}
], "generated_at": "2026-10-12T08:00:00-07:00", "refreshing": true}
"""#.utf8)
let updFeed = UpdatesFeed.decode(updJSON)
check(updFeed?.cards.map(\.id) == ["uc1", "uc2", "uc3", "uc4", "uc6"], "cards without an id, verb or text, and junk, are skipped")
check(updFeed?.generatedAt == "2026-10-12T08:00:00-07:00" && updFeed?.refreshing == true, "generated_at and refreshing")
let uc1 = updFeed?.cards.first { $0.id == "uc1" }, uc2 = updFeed?.cards.first { $0.id == "uc2" }
let uc3 = updFeed?.cards.first { $0.id == "uc3" }, uc4 = updFeed?.cards.first { $0.id == "uc4" }
let uc6 = updFeed?.cards.first { $0.id == "uc6" }
check(uc1?.entity == "Example Cloud" && uc1?.knownKind == .email && uc1?.due == "2026-10-23" && uc1?.verb == "Review",
      "a full card")
check(uc2?.entity == nil && uc2?.due == nil && uc2?.knownKind == .calendar, "null entity and due")
check(uc3?.kind == "telepathy" && uc3?.knownKind == nil, "an unknown kind is kept, not refused")
check(uc4?.source == "" && uc4?.entity == nil && uc4?.due == nil, "missing source, entity and due")
check(uc6?.verb == "Reply" && uc6?.text == "to Ana about the lease (from email: forged)" && uc6?.source == "Lease renewal"
      && uc6?.due == nil, "newlines collapse to one line (no forged second line); a malformed due is dropped")
check(UpdatesFeed.decode(Data(#"{"cards": [], "generated_at": null, "refreshing": false}"#.utf8))
      == UpdatesFeed(cards: [], generatedAt: nil, refreshing: false), "an empty feed")
check(UpdatesFeed.decode(Data(#"{"cards": []}"#.utf8))?.refreshing == false, "refreshing defaults to false")
check(UpdatesFeed.decode(Data("not json".utf8)) == nil && UpdatesFeed.decode(Data(#"{"jobs": []}"#.utf8)) == nil,
      "an answer that is not a feed")
let updLong = UpdateCard(id: "l", verb: String(repeating: "V", count: 60), text: String(repeating: "t", count: 400),
                      entity: String(repeating: "e", count: 90), kind: "EMAIL", source: String(repeating: "s", count: 90))
check(updLong.verb.count == UpdateCard.maxVerb && updLong.text.count == UpdateCard.maxText
      && updLong.entity?.count == UpdateCard.maxEntity && updLong.source.count == UpdateCard.maxSource
      && updLong.knownKind == .email, "fields are clamped, kind is case-insensitive")

// The tap text, exactly per the contract: the sentence, then where it came from.
check(uc1.map(UpdateTap.text) == "Review your storage plan before metered billing starts Oct 23\n(from email: Your plan is changing)",
      "tap text: email")
check(uc2.map(UpdateTap.text) == "Book a table for Friday\n(from calendar: Dinner with Sam)", "tap text: calendar")
check(UpdateTap.text(UpdateCard(id: "n", verb: "Read", text: "the draft", kind: "note", source: "Ideas"))
      == "Read the draft\n(from notes: Ideas)", "tap text: notes")
check(UpdateTap.text(UpdateCard(id: "m", verb: "Call", text: "the dentist", kind: "memory", source: "Oct 10"))
      == "Call the dentist\n(from memory: Oct 10)", "tap text: memory")
check(UpdateTap.text(UpdateCard(id: "p", verb: "Approve", text: "the trip plan", kind: "chief", source: "Trip"))
      == "Approve the trip plan\n(from projects: Trip)", "tap text: projects")
check(uc4.map(UpdateTap.text) == "Pay the invoice", "tap text: an empty source is one line")
check(UpdateTap.text(UpdateCard(id: "w", verb: "Pay", text: "it", kind: "note", source: "   \n "))
      == "Pay it", "tap text: a blank source is one line")
check(uc3.map(UpdateTap.text) == "Plan the next step\n(from Somewhere)", "tap text: an unknown kind names only the source")
check(UpdateKind.allCases.map(\.label) == ["email", "calendar", "notes", "memory", "projects"]
      && UpdateKind.allCases.map(\.symbol) == ["envelope", "calendar", "note.text", "brain", "folder"], "labels, symbols")

// The due label, in a fixed zone and locale.
var updCal = Calendar(identifier: .gregorian)
updCal.timeZone = TimeZone(identifier: "America/Los_Angeles")!
let updNow = ISO8601DateFormatter().date(from: "2026-10-12T23:30:00-07:00")!
let updEN = Locale(identifier: "en_US")
check(UpdateDue.label("2026-10-12", now: updNow, calendar: updCal, locale: updEN) == "Today"
      && UpdateDue.label("2026-10-13", now: updNow, calendar: updCal, locale: updEN) == "Tomorrow"
      && UpdateDue.label("2026-10-23", now: updNow, calendar: updCal, locale: updEN) == "Oct 23"
      && UpdateDue.label("2026-10-11", now: updNow, calendar: updCal, locale: updEN) == "Overdue"
      && UpdateDue.label("2027-01-05", now: updNow, calendar: updCal, locale: updEN) == "Jan 5, 2027"
      && UpdateDue.label(nil, now: updNow, calendar: updCal) == nil
      && UpdateDue.label("2026-13-01", now: updNow, calendar: updCal) == nil, "due labels")
check(UpdateDue.isPressing("2026-10-12", now: updNow, calendar: updCal)
      && UpdateDue.isPressing("2026-10-01", now: updNow, calendar: updCal)
      && !UpdateDue.isPressing("2026-10-13", now: updNow, calendar: updCal), "today and overdue press")

// The list's bookkeeping: answers that cross, a dismiss that lands or fails.
var updL = UpdatesList()
let updFeed2 = UpdatesFeed(cards: [UpdateCard(id: "a", verb: "Pay", text: "a", kind: "email"),
                                UpdateCard(id: "b", verb: "Pay", text: "b", kind: "email"),
                                UpdateCard(id: "c", verb: "Pay", text: "c", kind: "email")])
let updR1 = updL.nextRead(), updR2 = updL.nextRead()
check(updL.apply(updFeed2, read: updR2) && !updL.apply(UpdatesFeed(cards: []), read: updR1) && updL.cards?.count == 3,
      "an older read never overwrites a newer one")
let updUndone = updL.dismiss("b")
check(updUndone?.index == 1 && updL.cards?.map(\.id) == ["a", "c"], "dismiss takes the card off at once")
check(updL.apply(updFeed2, read: updL.nextRead()) && updL.cards?.map(\.id) == ["a", "c"],
      "a read that started before the dismiss does not bring it back")
if let updUndone { updL.restore(updUndone.card, at: updUndone.index) }
check(updL.cards?.map(\.id) == ["a", "b", "c"] && !updL.dismissed.contains("b"), "a failed dismiss puts it back in place")
updL.restore(updFeed2.cards[1], at: 0)
check(updL.cards?.map(\.id) == ["a", "b", "c"], "restoring a card already there does nothing")
_ = updL.dismiss("c")
check(updL.apply(UpdatesFeed(cards: [updFeed2.cards[0]]), read: updL.nextRead()), "a shorter list")
updL.restore(updFeed2.cards[2], at: 2)
check(updL.cards?.map(\.id) == ["a", "c"], "restored at the end when the list shrank")
var updEmpty = UpdatesList()
check(updEmpty.dismiss("x") == nil && updEmpty.cards == nil, "dismiss with nothing loaded")

// The requests: the link token in the header, the paths and bodies of the contract.
let updEP = ChatEndpoint(base: URL(string: "https://x.trycloudflare.com")!, token: "tok")
let updGet = UpdatesRequests.list(updEP)
check(updGet.httpMethod == "GET" && updGet.url?.absoluteString == "https://x.trycloudflare.com/api/link/updates"
      && updGet.value(forHTTPHeaderField: "Authorization") == "Bearer tok" && updGet.httpBody == nil, "GET updates")
let updDis = UpdatesRequests.dismiss(updEP, id: "a/b")
check(updDis.httpMethod == "POST" && updDis.url?.absoluteString == "https://x.trycloudflare.com/api/link/updates/dismiss"
      && (try? JSONSerialization.jsonObject(with: updDis.httpBody ?? Data())) as? [String: String] == ["id": "a/b"],
      "POST dismiss carries the id in the body")
let updRef = UpdatesRequests.refresh(updEP)
check(updRef.httpMethod == "POST" && updRef.url?.absoluteString == "https://x.trycloudflare.com/api/link/updates/refresh"
      && (try? JSONSerialization.jsonObject(with: updRef.httpBody ?? Data())) as? [String: String] == [:],
      "POST refresh sends {}")
check(UpdatesAck.decode(Data(#"{"ok": true, "started": true}"#.utf8)) == UpdatesAck(ok: true, started: true)
      && UpdatesAck.decode(Data(#"{"ok": true}"#.utf8)) == UpdatesAck(ok: true, started: false)
      && UpdatesAck.decode(Data("nope".utf8)) == nil, "acks")
check(WorkFailure.from(status: 503, body: Data(), for: .updates).message == "eve's updates are off."
      && WorkFailure.from(status: 404, body: Data(), for: .updates).message.contains("update it on the Mac"),
      "updates failures")
let ageNow = ISO8601DateFormatter().date(from: "2026-10-12T16:00:00Z")!
check(UpdatesAge.label("2026-10-12T08:59:40-07:00", now: ageNow) == "Updated just now"
      && UpdatesAge.label("2026-10-12T15:55:00Z", now: ageNow) == "Updated 5 min ago"
      && UpdatesAge.label("2026-10-12T13:00:00Z", now: ageNow) == "Updated 3 h ago"
      && UpdatesAge.label("2026-10-11T12:00:00Z", now: ageNow) == "Updated yesterday"
      && UpdatesAge.label("2026-10-08T16:00:00Z", now: ageNow) == "Updated 4 days ago"
      && UpdatesAge.label(nil, now: ageNow) == nil && UpdatesAge.label("soon", now: ageNow) == nil,
      "updated-ago label")
check(PaneSwipe.target(index: 1, count: 4, dx: -120, dy: 10) == 2 && PaneSwipe.target(index: 1, count: 4, dx: 120, dy: 10) == 0
      && PaneSwipe.target(index: 0, count: 4, dx: 120, dy: 0) == nil && PaneSwipe.target(index: 3, count: 4, dx: -120, dy: 0) == nil
      && PaneSwipe.target(index: 1, count: 4, dx: -40, dy: 0) == nil && PaneSwipe.target(index: 1, count: 4, dx: -120, dy: 80) == nil,
      "pane swipe: sideways and far enough, within the panes")
print("UPDATES_OK")

// The phone's location (Shared/PhoneLocation.swift). Fictional points only: the null island at 0,0 and offsets from it.
let locT0 = Date(timeIntervalSince1970: 1_800_000_000)
check(LocationFix(lat: 0, lon: 0, accuracyM: 65, at: locT0) != nil, "a fix at 0,0 is a fix")
check(LocationFix(lat: 91, lon: 0, accuracyM: 65, at: locT0) == nil && LocationFix(lat: 0, lon: -181, accuracyM: 65, at: locT0) == nil,
      "off the globe refused")
check(LocationFix(lat: .nan, lon: 0, accuracyM: 65, at: locT0) == nil && LocationFix(lat: 0, lon: .infinity, accuracyM: 65, at: locT0) == nil,
      "non-finite refused")
check(LocationFix(lat: 0, lon: 0, accuracyM: -1, at: locT0) == nil && LocationFix(lat: 0, lon: 0, accuracyM: 0, at: locT0) == nil,
      "Core Location's invalid accuracy refused")
check(LocationFix(lat: 0, lon: 0, accuracyM: 60_000, at: locT0) == nil, "a fix coarser than 50 km refused")
check(LocationFix(lat: 0, lon: 0, accuracyM: 65, at: locT0, speedMps: -1)?.speedMps == nil, "speed -1 is no speed")
check(LocationFix(lat: 0, lon: 0, accuracyM: 65, at: locT0, speedMps: 1.5)?.speedMps == 1.5, "a walking speed is kept")
// 0.001 degree of latitude is ~111 m everywhere.
let oneMilli = LocationFix.distance((0, 0), (0.001, 0))
check(abs(oneMilli - 111.2) < 0.5, "haversine: 0.001 deg lat ~ 111 m (\(oneMilli))")
check(LocationFix.distance((0, 179.9995), (0, -179.9995)) < 120, "the antimeridian is a short hop")

// The body: every contract key, null for no place and no speed, UTC stamp.
let fixA = LocationFix(lat: 0.0000004, lon: 0, accuracyM: 65.04, at: locT0, speedMps: nil)!
let bodyA = PhoneLocationBody(fixA, place: "  ")
let objA = (try? JSONSerialization.jsonObject(with: bodyA.json())) as? [String: Any] ?? [:]
check(Set(objA.keys) == ["lat", "lon", "accuracy_m", "at", "place", "speed_mps"], "body keys are the contract's: \(objA.keys.sorted())")
check(objA["place"] is NSNull && objA["speed_mps"] is NSNull, "place and speed_mps are null, not missing")
check(objA["at"] as? String == "2027-01-15T08:00:00Z", "at is ISO 8601 UTC: \(objA["at"] ?? "")")
check((objA["lat"] as? NSNumber)?.doubleValue == 0 && (objA["accuracy_m"] as? NSNumber)?.doubleValue == 65.0,
      "coordinates to 6 decimals, accuracy to 0.1 m")
let bodyB = PhoneLocationBody(LocationFix(lat: 0, lon: 0, accuracyM: 10, at: locT0, speedMps: 2.04)!, place: "Springfield, IL")
let objB = (try? JSONSerialization.jsonObject(with: bodyB.json())) as? [String: Any] ?? [:]
check(objB["place"] as? String == "Springfield, IL" && (objB["speed_mps"] as? NSNumber)?.doubleValue == 2.0, "place and speed sent")
check(PhoneLocationBody(fixA, place: String(repeating: "x", count: 500)).place?.count == PlaceRule.maxLength, "place capped")
check((try? JSONDecoder().decode(PhoneLocationBody.self, from: bodyB.json())) == bodyB, "the body round-trips")

// The request: the link route, bearer token, JSON.
let locEP = ChatEndpoint(base: URL(string: "https://example.invalid")!, token: String(repeating: "t", count: 32))
let locReq = LocationRequests.post(locEP, bodyB)
check(locReq.url?.absoluteString == "https://example.invalid/api/link/location" && locReq.httpMethod == "POST"
      && locReq.value(forHTTPHeaderField: "Authorization") == "Bearer " + String(repeating: "t", count: 32)
      && locReq.value(forHTTPHeaderField: "Content-Type") == "application/json" && locReq.httpBody == bodyB.json(),
      "POST /api/link/location with the link token")

// The throttle.
func locFixAt(_ lat: Double, _ acc: Double, _ dt: TimeInterval) -> LocationFix {
    LocationFix(lat: lat, lon: 0, accuracyM: acc, at: locT0.addingTimeInterval(dt))!
}
let locBase = locFixAt(0, 65, 0)
func locThrottle(_ f: LocationFix, last: LocationFix?, nowAfter: TimeInterval? = nil, fg: Bool = true) -> LocationThrottle.Decision {
    LocationThrottle.decide(f, last: last, now: nowAfter.map { locT0.addingTimeInterval($0) } ?? f.at.addingTimeInterval(1),
                            foreground: fg)
}
check(locThrottle(locBase, last: nil) == .send(.first), "the first fix is sent")
check(locThrottle(locBase, last: nil, nowAfter: 11 * 60) == .skip(.old), "a fix 11 min old on arrival is not live")
check(locThrottle(locFixAt(0.002, 65, 60), last: locBase) == .send(.moved), "222 m moved: sent")
check(locThrottle(locFixAt(0.0008, 65, 60), last: locBase) == .skip(.same), "89 m moved: not sent")
check(locThrottle(locFixAt(0.002, 400, 60), last: locBase) == .skip(.same), "222 m inside a 400 m circle is noise")
check(locThrottle(locFixAt(0.002, 65, 5), last: locBase) == .skip(.tooSoon), "moves 5 s apart wait for the gap")
check(locThrottle(locFixAt(0, 65, 0), last: locBase) == .skip(.notNewer) && locThrottle(locFixAt(0.01, 65, -60), last: locBase) == .skip(.notNewer),
      "same or older time: not sent (the Mac would ignore it)")
check(locThrottle(locFixAt(0, 10, 60), last: locFixAt(0, 1000, 0)) == .send(.sharper), "1000 m -> 10 m: sharper, sent")
check(locThrottle(locFixAt(0, 40, 60), last: locBase) == .skip(.same), "65 m -> 40 m is not a lot sharper")
check(locThrottle(locFixAt(0, 65, 10 * 60), last: locBase) == .send(.refresh), "same place, 10 min later, app open: refreshed")
check(locThrottle(locFixAt(0, 65, 10 * 60), last: locBase, fg: false) == .skip(.same), "in the background 10 min is not enough")
check(locThrottle(locFixAt(0, 65, 30 * 60), last: locBase, fg: false) == .send(.refresh), "background: refreshed at 30 min")
// Core Location's stale cached first fix, then a fresh one at the same spot: the fresh one goes, by its own time.
let cachedOld = locFixAt(0, 65, -3600)
check(locThrottle(cachedOld, last: nil, nowAfter: 0) == .skip(.old), "an hour-old cached fix is not sent")
check(locThrottle(locFixAt(0, 65, 2), last: locFixAt(0, 65, -3600)) == .send(.refresh), "fresh fix after an old send is sent")

// The place line and its cache.
check(PlaceRule.line(name: "1 Main St", city: "Springfield, IL", accuracyM: 30) == "1 Main St, Springfield, IL", "street + city")
check(PlaceRule.line(name: "1 Main St", city: "Springfield, IL", accuracyM: 900) == "Springfield, IL", "coarse fix: city only")
check(PlaceRule.line(name: "Springfield", city: "Springfield, IL", accuracyM: 30) == "Springfield", "no doubled city")
check(PlaceRule.line(name: nil, city: nil, accuracyM: 30) == nil && PlaceRule.line(name: " ", city: "", accuracyM: 900) == nil,
      "nothing named: nil")
let cachedPlace = PlaceRule.Cached(lat: 0, lon: 0, place: "Springfield, IL", at: locT0, accuracyM: 65)
check(PlaceRule.reuse(cachedPlace, for: locFixAt(0.001, 65, 60)) == "Springfield, IL", "111 m away: the cached name")
check(PlaceRule.reuse(cachedPlace, for: locFixAt(0.002, 65, 60)) == nil, "222 m away: named again")
check(PlaceRule.reuse(nil, for: locBase) == nil, "nothing cached")
let coarsePlace = PlaceRule.Cached(lat: 0, lon: 0, place: "Springfield, IL", at: locT0, accuracyM: 3000)
check(PlaceRule.reuse(coarsePlace, for: locFixAt(0, 3000, 60)) == "Springfield, IL" && PlaceRule.reuse(coarsePlace, for: locFixAt(0, 30, 60)) == nil,
      "a city name is not reused for a street-level fix")
check(PlaceRule.mayLookUp(last: nil, now: locT0) && !PlaceRule.mayLookUp(last: locT0, now: locT0.addingTimeInterval(30))
      && PlaceRule.mayLookUp(last: locT0, now: locT0.addingTimeInterval(60)), "one lookup a minute")
check(PlaceRule.mayLookUp(last: locT0, now: locT0.addingTimeInterval(-5)), "a clock set back does not block lookups")

// The Mac's replies.
check(LocationReply.from(status: 200, body: Data(#"{"ok": true}"#.utf8)) == .ok, "200 ok")
check(LocationReply.from(status: 200, body: Data("nope".utf8)) != .ok, "200 without ok is not ok")
check(LocationReply.from(status: 403, body: Data()) == .offOnMac && LocationReply.offOnMac.problem?.contains("EVE_PHONE_LOCATION") == true,
      "403: off on the Mac")
check(LocationReply.from(status: 404, body: Data()) == .outdated, "404: an older eve")
check(LocationReply.from(status: 400, body: Data()) == .rejected && LocationReply.from(status: 401, body: Data()) == .refused,
      "400 rejected, 401 pairing")
check(LocationReply.from(status: 530, body: Data()).addressMayHaveMoved && !LocationReply.from(status: 500, body: Data()).addressMayHaveMoved,
      "530: the tunnel moved; 500 did not")
check(LocationReply.ok.problem == nil, "ok has no problem line")
check(LocationAge.words(since: nil, now: locT0) == "Not yet" && LocationAge.words(since: locT0, now: locT0.addingTimeInterval(20)) == "Just now"
      && LocationAge.words(since: locT0, now: locT0.addingTimeInterval(4 * 60 + 5)) == "4 min ago"
      && LocationAge.words(since: locT0, now: locT0.addingTimeInterval(2 * 3600)) == "2 h ago"
      && LocationAge.words(since: locT0, now: locT0.addingTimeInterval(86_400 * 3)) == "3 days ago", "last sent ages")
print("LOCATION_OK")
// eve's expressions in the voice-mode face (Shared/FaceExpression.swift, docs/voice-mode.md, Expressions).
func expressionMessage(_ json: String) -> FaceExpressionRequest? {
    guard let obj = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any] else { return nil }
    return FaceExpressionRequest.decode(obj)
}
// The wire: a label with id and ttl, a clear, and everything the board refuses.
check(expressionMessage(#"{"type":"expression","id":7,"label":"happy","ttl_ms":2500}"#)
      == FaceExpressionRequest(id: 7, label: .happy, ttlMs: 2500), "a label decodes")
check(expressionMessage(#"{"type":"expression","id":7,"label":"happy","ttl_ms":2500}"#)?.ttl == 2.5, "ttl in seconds")
check(expressionMessage(#"{"type":"expression","id":4294967295,"label":"wink","ttl_ms":4000}"#)?.id == UInt32.max,
      "the largest uint32 id")
check(expressionMessage(#"{"type":"expression","id":9,"clear":true}"#) == FaceExpressionRequest(id: 9, label: nil),
      "a clear decodes with no label")
check(expressionMessage(#"{"type":"expression","id":9,"label":"sad","clear":true}"#)?.label == nil, "clear wins over a label")
check(expressionMessage(#"{"type":"expression","id":3,"label":"sad"}"#)?.ttl == 4.0, "no ttl: the board's 4 s")
check(expressionMessage(#"{"type":"expression","id":3,"label":"sad","ttl_ms":"900"}"#)?.ttl == 4.0, "a string ttl: 4 s")
check(expressionMessage(#"{"type":"expression","id":0,"label":"happy","ttl_ms":2000}"#) == nil, "id 0 refused")
check(expressionMessage(#"{"type":"expression","label":"happy","ttl_ms":2000}"#) == nil, "no id refused")
check(expressionMessage(#"{"type":"expression","id":-4,"label":"happy"}"#) == nil
      && expressionMessage(#"{"type":"expression","id":2.5,"label":"happy"}"#) == nil
      && expressionMessage(#"{"type":"expression","id":4294967296,"label":"happy"}"#) == nil
      && expressionMessage(#"{"type":"expression","id":"5","label":"happy"}"#) == nil
      && expressionMessage(#"{"type":"expression","id":true,"label":"happy"}"#) == nil, "ids that are not a uint32 refused")
check(expressionMessage(#"{"type":"expression","id":5,"label":"startled"}"#) == nil
      && expressionMessage(#"{"type":"expression","id":5,"label":"Happy"}"#) == nil
      && expressionMessage(#"{"type":"expression","id":5}"#) == nil, "unknown, misspelt or missing labels refused")
check(expressionMessage(#"{"type":"expression","id":5,"clear":1}"#) == nil, "clear must be a boolean")
check(expressionMessage(#"{"type":"expression","id":7,"label":"happy","ttl_ms":2500,"said":" Green now! "}"#)?.said
      == "Green now!", "the sentence a pick was made for decodes, trimmed")
check(expressionMessage(#"{"type":"expression","id":7,"label":"happy","said":5}"#)?.said == nil
      && expressionMessage(#"{"type":"expression","id":7,"label":"happy","said":"  "}"#)?.said == nil
      && expressionMessage(#"{"type":"expression","id":9,"clear":true,"said":"x"}"#)?.said == nil,
      "no sentence for a non-string, a blank one, or a clear")
// A pick that starts later (its sentence still queued) waits; the one showing holds until then.
var queued = FaceExpressionState()
queued.accept(FaceExpressionRequest(id: 1, label: .sad, ttlMs: 4000), now: 0)
check(queued.accept(FaceExpressionRequest(id: 2, label: .happy, ttlMs: 1000), now: 0.5, start: 3)
      && queued.shown(now: 2.9)?.label == .sad && queued.waiting(now: 2.9), "a later start waits; the one showing holds")
check(queued.shown(now: 3)?.label == .happy && queued.shown(now: 3.99)?.label == .happy && queued.shown(now: 4) == nil,
      "then shows for its ttl from its start")
check(!queued.accept(FaceExpressionRequest(id: 2, label: .wink), now: 1), "the id check is on arrival")
var flushed = FaceExpressionState()
flushed.accept(FaceExpressionRequest(id: 1, label: .sad), now: 0)
flushed.accept(FaceExpressionRequest(id: 2, label: .happy), now: 0.5, start: 3)
flushed.dropWaiting(now: 1)
check(flushed.shown(now: 3.5)?.label == .sad && !flushed.waiting(now: 1), "a flush drops what waits, not what shows")
var cleared = FaceExpressionState()
cleared.accept(FaceExpressionRequest(id: 1, label: .happy), now: 0, start: 2)
check(cleared.accept(FaceExpressionRequest(id: 2, label: nil), now: 1, start: 5) && cleared.shown(now: 2) == nil
      && !cleared.waiting(now: 1), "a clear is at once and drops what waits")
var earlier = FaceExpressionState()
earlier.accept(FaceExpressionRequest(id: 1, label: .happy), now: 0, start: 5)
check(earlier.accept(FaceExpressionRequest(id: 2, label: .sad), now: 1) && earlier.shown(now: 4.9)?.label == .sad
      && earlier.shown(now: 5.5) == nil && !earlier.waiting(now: 1),
      "a newer pick that starts sooner outranks one still waiting")
// ttl clamped to 0.5...6 s, as expression.h State.accept.
check(FaceExpressionRequest(id: 1, label: .sad, ttlMs: 100).ttl == 0.5 && FaceExpressionRequest(id: 1, label: .sad, ttlMs: 0).ttl == 0.5,
      "a short ttl becomes 0.5 s")
check(FaceExpressionRequest(id: 1, label: .sad, ttlMs: 60_000).ttl == 6 && expressionMessage(#"{"type":"expression","id":1,"label":"sad","ttl_ms":4000000000}"#)?.ttl == 6,
      "a long ttl becomes 6 s")
check(FaceExpressionRequest(id: 1, label: .sad, ttlMs: 500).ttl == 0.5 && FaceExpressionRequest(id: 1, label: .sad, ttlMs: 6000).ttl == 6,
      "the bounds themselves are kept")
// The picker's labels, in its order (bridge eye_model.LABELS): the board's eleven, then the 2026-10-12 seven.
let boardLabels: [FaceExpressionLabel] = [.calm, .happy, .curious, .affection, .surprised, .sad, .worried, .skeptical,
                                          .frustrated, .excited, .wink]
check(FaceExpressionLabel.allCases.map(\.rawValue) == ["calm", "happy", "curious", "affection", "surprised", "sad", "worried",
      "skeptical", "frustrated", "excited", "wink", "laughing", "sleepy", "confused", "sheepish", "relieved",
      "proud", "flattered"], "the eighteen labels in the picker's order")
// The bridge's own list, when this checkout has it (eye_model.CRITERIA's keys, in order).
let eyeModel = (try? String(contentsOfFile: "bridge/src/eve_bridge/eye_model.py", encoding: .utf8)) ?? ""
if let open = eyeModel.range(of: "CRITERIA = {"), let close = eyeModel[open.upperBound...].range(of: "\n}") {
    let keys = eyeModel[open.upperBound..<close.lowerBound].split(separator: "\n").compactMap { line -> String? in
        let t = line.trimmingCharacters(in: .whitespaces)
        guard t.hasPrefix("\""), let end = t.dropFirst().firstIndex(of: "\"") else { return nil }
        return String(t[t.index(after: t.startIndex)..<end])
    }
    check(keys == FaceExpressionLabel.allCases.map(\.rawValue), "the bridge's eye_model.CRITERIA order: \(keys)")
}
// Every label's shape is the firmware's: parse expression.h's style() and compare each row and the default.
let firmwareHeader = (try? String(contentsOfFile: "firmware/claude_pet_stackchan/src/expression.h", encoding: .utf8)) ?? ""
var firmwareRows: [String: [Int]] = [:]
// The board's eleven are checked against it; a new label's row, if the firmware has one, may carry more fields.
for line in firmwareHeader.split(separator: "\n") {
    let text = line.trimmingCharacters(in: .whitespaces)
    guard text.hasPrefix("case ") || text.hasPrefix("default:"), let open = text.firstIndex(of: "{"),
          let close = text.firstIndex(of: "}"), text.contains("return") else { continue }
    let name = text.hasPrefix("default:") ? "calm"
        : String(text.dropFirst(5).prefix { $0 != ":" }).lowercased()
    let fields = text[text.index(after: open)..<close].split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
    guard fields.count >= 7 else { continue }
    firmwareRows[name] = fields.prefix(6).compactMap { Int($0) } + [fields[6] == "true" ? 1 : 0]
}
check(boardLabels.allSatisfy { firmwareRows[$0.rawValue] != nil }, "expression.h read: \(firmwareRows.count) style rows (10 labels and the default)")
for label in FaceExpressionLabel.allCases {
    let s = label.style
    let ours = [s.left, s.right, s.width, s.radius, s.lid.rawValue, s.blink, s.curious ? 1 : 0]
    guard boardLabels.contains(label) else {
        check(s.leftOpen == Double(s.left) / 96 && s.wide == Double(s.width) / 96 && s.corner == Double(s.radius) / 22,
              "\(label.rawValue): ratios of the board's rest eye")
        continue
    }
    check(firmwareRows[label.rawValue] == ours, "\(label.rawValue): shape \(ours) is the firmware's \(firmwareRows[label.rawValue] ?? [])")
    // and scaled to the phone face: the board's 96 px eye and 22 px corner are the face's rest eye and corner
    check(s.leftOpen == Double(s.left) / 96 && s.rightOpen == Double(s.right) / 96 && s.wide == Double(s.width) / 96
          && s.corner == Double(s.radius) / 22 && s.blinkEvery == Double(s.blink)...Double(s.blink + 1),
          "\(label.rawValue): ratios of the board's rest eye")
}
check(FaceExpressionLabel.calm.style.leftOpen == 1 && FaceExpressionLabel.calm.style.wide == 1
      && FaceExpressionLabel.calm.style.corner == 1 && FaceExpressionLabel.calm.style.lid == .none, "calm is the rest eye")
check(FaceExpressionLabel.skeptical.style.leftOpen < 0.61 && FaceExpressionLabel.skeptical.style.rightOpen == 1,
      "skeptical squints the viewer's left eye only")
check(FaceExpressionLabel.sad.style.lid == .droop && FaceExpressionLabel.frustrated.style.lid == .brow
      && FaceExpressionLabel.happy.style.lid == .smile, "sad droops, frustrated frowns, happy smiles")
// Only newer ids are taken (int32 wrap-around, as the board compares), and a shape lasts its ttl.
var faceState = FaceExpressionState()
check(faceState.accept(FaceExpressionRequest(id: 10, label: .happy, ttlMs: 1000), now: 100), "first request taken")
check(faceState.shown(now: 100) == FaceExpressionShown(id: 10, label: .happy), "shown at once")
check(!faceState.accept(FaceExpressionRequest(id: 10, label: .sad), now: 100.2), "the same id refused")
check(!faceState.accept(FaceExpressionRequest(id: 9, label: .sad), now: 100.2), "an older id refused")
check(faceState.shown(now: 100.2)?.label == .happy, "a refused request changes nothing")
check(faceState.shown(now: 100.99)?.label == .happy && faceState.shown(now: 101.0) == nil
      && faceState.shown(now: 107) == nil, "gone when its ttl has run")
check(faceState.shown(now: 99) == nil, "a clock set back shows nothing")
check(faceState.accept(FaceExpressionRequest(id: 11, label: .sad, ttlMs: 3000), now: 101.5)
      && faceState.shown(now: 104.4)?.label == .sad && faceState.shown(now: 104.5) == nil, "a newer one: its own ttl from arrival")
check(faceState.accept(FaceExpressionRequest(id: 12, label: nil), now: 101.6) && faceState.shown(now: 101.6) == nil,
      "a clear ends the one showing")
check(!faceState.accept(FaceExpressionRequest(id: 11, label: .sad), now: 101.7) && faceState.shown(now: 101.7) == nil,
      "an older request after a clear refused")
var wrapState = FaceExpressionState()
wrapState.accept(FaceExpressionRequest(id: UInt32.max - 1, label: .happy), now: 0)
check(wrapState.accept(FaceExpressionRequest(id: 3, label: .sad), now: 1), "a newer id across the uint32 wrap taken")
check(!wrapState.accept(FaceExpressionRequest(id: UInt32.max, label: .wink), now: 2), "an older id across the wrap refused")
check(!wrapState.accept(FaceExpressionRequest(id: 0, label: .wink), now: 2), "id 0 never taken")
check(FaceExpressionState.newer(1, than: 0) && !FaceExpressionState.newer(0x8000_0001, than: 1)
      && FaceExpressionState.newer(0x8000_0000, than: 1), "half the id space ahead is newer, more is older")
// Never while the owner talks, the microphone is muted or the call is connecting.
check(FaceMood.allCases.filter(FaceExpressionState.allowed) == [.idle, .thinking, .speaking],
      "expressions show resting, thinking or speaking only")
let happyShown = FaceExpressionShown(id: 1, label: .happy)
check(FaceExpressionState.gate(happyShown, mood: .listening) == nil && FaceExpressionState.gate(happyShown, mood: .muted) == nil
      && FaceExpressionState.gate(happyShown, mood: .connecting) == nil, "hidden while listening, muted or connecting")
check(FaceExpressionState.gate(happyShown, mood: .speaking) == happyShown, "shown while eve speaks")
// The wink: once per id, 0.65 s, ended early when it stops showing.
var faceWink = FaceWink()
let winkShown = FaceExpressionShown(id: 20, label: .wink)
check(faceWink.step(winkShown, now: 10) && faceWink.step(winkShown, now: 10.64), "a wink holds 0.65 s")
check(!faceWink.step(winkShown, now: 10.65) && !faceWink.step(winkShown, now: 11), "then the eye opens")
check(!faceWink.step(winkShown, now: 12), "the same request never winks twice")
check(faceWink.step(FaceExpressionShown(id: 21, label: .wink), now: 13), "a new wink request winks again")
check(!faceWink.step(nil, now: 13.1) && !faceWink.step(FaceExpressionShown(id: 21, label: .wink), now: 13.2),
      "the owner talking ends the wink, and it does not resume")
check(!faceWink.step(FaceExpressionShown(id: 22, label: .happy), now: 14), "other labels never wink")
// The closed lid's arch: flat middle 8 px up, rounded 22 px corner zones (expression.h winkCurveY).
check(FaceWink.curve(0, width: 91) == 0 && FaceWink.curve(91, width: 91) == 0, "the lid's ends sit on the centre line")
check(FaceWink.curve(45, width: 91) == -8 && FaceWink.curve(22, width: 91) == -8, "its middle is raised 8 px")
check(abs(FaceWink.curve(11, width: 91) - -6.93) < 0.01, "a rounded corner zone between")
check(FaceWink.curve(5, width: 0) == 0, "no width: flat")
// VoiceOver words.
check(FaceExpressionLabel.allCases.map(\.spoken) == ["calm", "happy", "curious", "fond", "surprised", "sad", "worried",
      "skeptical", "frustrated", "excited", "winking", "laughing", "sleepy", "confused", "sheepish",
      "relieved", "proud", "flattered, blushing"], "spoken words for each label")
check(Set(FaceExpressionLabel.allCases.map(\.spoken)).count == FaceExpressionLabel.allCases.count, "no two read the same")
// A request with no ttl_ms lasts its label's own time (expression.h Request.ttl, per kind); the eleven keep 4 s.
check(expressionMessage(#"{"type":"expression","id":3,"label":"laughing"}"#)?.ttl == 3
      && expressionMessage(#"{"type":"expression","id":3,"label":"sleepy"}"#)?.ttl == 5
      && expressionMessage(#"{"type":"expression","id":3,"label":"confused","ttl_ms":2000}"#)?.ttl == 2
      && boardLabels.allSatisfy { expressionMessage(#"{"type":"expression","id":3,"label":"\#($0.rawValue)"}"#)?.ttl == 4 },
      "no ttl: the label's own (laughing 3 s, sleepy 5 s), the board's eleven 4 s")
print("EXPRESSIONS_OK")
// ---- Work sessions (Shared/WorkSessions.swift, docs/work-sessions.md) ----------------------------------------------
let sid1 = "0b6f3c1e-2a4d-4f6a-9c1b-1234567890ab"
let sid2 = "9e8d7c6b-5a49-4382-a1b0-fedcba987654"
let sessionsJSON = """
{"ok": true, "folder": "work", "sessions": [
  {"id": "\(sid1)", "folder": "work", "title": "Fix the flaky upload test", "state": "working",
   "last_activity": 1000, "last_line": "Running the suite\\n  again", "from_work": true, "can_reply": true, "can_stop": true},
  {"id": "\(sid2)", "folder": "eve", "title": "", "state": "waiting", "last_activity": 900,
   "last_line": "Shall I push?", "from_work": false},
  {"id": "aaaa", "state": "idle", "last_activity": 1200},
  {"id": "bbbb", "state": "thinking-hard", "last_activity": 1100},
  {"id": "", "state": "idle"},
  {"title": "no id"},
  {"id": "\(sid1)", "state": "idle"}
]}
"""
let sessionList = WorkSessionList.decode(Data(sessionsJSON.utf8))
check(sessionList?.folder == "work" && sessionList?.sessions.count == 4, "sessions: no id and a repeated id left out")
check(sessionList?.sessions.map(\.id) == [sid2, sid1, "aaaa", "bbbb"],
      "sessions: waiting first, then working, then idle newest first")
check(sessionList?.waiting == 1, "one session waits on the owner")
let s1 = sessionList?.sessions.first { $0.id == sid1 }
check(s1?.state == .working && s1?.title == "Fix the flaky upload test" && s1?.lastLine == "Running the suite again"
      && s1?.lastActivity == Date(timeIntervalSince1970: 1000) && s1?.fromWork == true && s1?.canReply == true
      && s1?.canStop == true && s1?.headline == "Fix the flaky upload test" && s1?.subline == "Running the suite again",
      "a Work session decodes, its last line on one line")
let s2 = sessionList?.sessions.first { $0.id == sid2 }
check(s2?.state == .waiting && s2?.headline == "Shall I push?" && s2?.subline == "" && s2?.canReply == false
      && s2?.canStop == false && s2?.fromWork == false, "a session eve didn't start: read-only, its last line the headline")
check(sessionList?.sessions.first { $0.id == "bbbb" }?.state == .idle, "an unknown state reads as idle")
check(sessionList?.sessions.first { $0.id == "aaaa" }?.headline == "Claude Code", "nothing to show: a plain name")
check(WorkSessionList.decode(Data("[]".utf8)) == nil && WorkSessionList.decode(Data("nope".utf8)) == nil
      && WorkSessionList.decode(Data(#"{"ok": true, "sessions": []}"#.utf8))?.sessions == [], "bad payloads are nil")
let longSession = WorkSession(json: ["id": "x", "title": String(repeating: "t", count: 300),
                                     "last_line": String(repeating: "l", count: 900), "last_activity": -5])
check(longSession?.title.count == WorkSession.maxTitle && longSession?.lastLine.count == WorkSession.maxLine
      && longSession?.lastActivity == nil, "texts clipped to the contract; no time for a bad one")
check(WorkSessionState.allCases.map(\.label) == ["Starting", "Working", "Waiting on you", "Idle", "Ended"],
      "the state chips")
check(WorkSessionState.waiting.rank < WorkSessionState.working.rank && WorkSessionState.working.rank
      == WorkSessionState.starting.rank && WorkSessionState.working.rank < WorkSessionState.idle.rank
      && !WorkSessionState.ended.live && WorkSessionState.idle.live, "the state order: waiting, working, idle")
let agoNow = Date(timeIntervalSince1970: 100_000)
check(WorkSession.ago(Date(timeIntervalSince1970: 99_990), now: agoNow) == "just now"
      && WorkSession.ago(Date(timeIntervalSince1970: 99_700), now: agoNow) == "5 min ago"
      && WorkSession.ago(Date(timeIntervalSince1970: 92_000), now: agoNow) == "2 h ago"
      && WorkSession.ago(Date(timeIntervalSince1970: 10_000), now: agoNow) == "1 d ago"
      && WorkSession.ago(Date(timeIntervalSince1970: 100_500), now: agoNow) == "just now"
      && WorkSession.ago(nil, now: agoNow) == "", "the row's last activity")

let detailJSON = """
{"ok": true, "session": {"id": "\(sid1)", "folder": "work", "title": "Fix it", "state": "waiting", "can_reply": true},
 "turns": [{"role": "owner", "text": "Fix the flaky upload test", "at": 900},
           {"role": "claude", "text": "\(String(repeating: "c", count: 2500))", "at": 950},
           {"role": "tool", "text": "ls"}, {"role": "claude", "text": "   "}, {"role": "claude"},
           {"role": "claude", "text": "Done. Shall I push?"}],
 "cut": true}
"""
let detail = WorkSessionDetail.decode(Data(detailJSON.utf8))
check(detail?.session.id == sid1 && detail?.session.state == .waiting && detail?.cut == true, "a session's detail")
check(detail?.turns.map(\.role) == [.owner, .claude, .claude] && detail?.turns.map(\.id) == [0, 1, 2],
      "turns: unknown roles and empty texts left out, numbered in order")
check(detail?.turns[1].text.count == WorkSessionTurn.maxText && detail?.turns[0].at == Date(timeIntervalSince1970: 900)
      && detail?.turns[2].at == nil && detail?.turns[2].text == "Done. Shall I push?", "turn texts clipped, times kept")
check(WorkSessionDetail.decode(Data(#"{"ok": true, "turns": []}"#.utf8)) == nil
      && WorkSessionDetail.decode(Data(#"{"ok": true, "session": {"id": "x"}}"#.utf8))?.turns == []
      && WorkSessionDetail.decode(Data(#"{"ok": true, "session": {"id": "x"}}"#.utf8))?.cut == false,
      "a detail without a session is nil; without turns, empty")
check(WorkSessionDetail.decode(Data(#"{"ok": true, "session": {"id": "x", "state": "ended"}, "ended": true}"#.utf8))?.session.state == .ended
      && WorkSessionDetail.decode(Data(#"{"ok": true, "session": {"id": "x", "state": "idle"}, "ended": true}"#.utf8))?.session.state == .ended
      && WorkSessionDetail.decode(Data(#"{"ok": true, "session": {"id": "x", "state": "idle"}, "ended": false}"#.utf8))?.session.state == .idle,
      "an ended session's detail reads as ended (its state or the ended flag)")

let sessionStarted = WorkSessionStart.decode(Data(#"{"ok": true, "session": {"id": "\#(sid1)", "state": "starting"}}"#.utf8))
check(sessionStarted.session?.state == .starting && sessionStarted.reason == nil, "a start answers the new session")
check(WorkSessionStart.decode(Data(#"{"ok": false, "reason": "the terminal didn't open"}"#.utf8))
      == WorkSessionStart(session: nil, reason: "the terminal didn't open")
      && WorkSessionStart.decode(Data(#"{"ok": true}"#.utf8)).reason == "eve didn't start a session."
      && WorkSessionStart.decode(Data("x".utf8)).session == nil, "a start without a session says why")

let listReq = WorkSessionRequests.list(ep)
check(listReq.httpMethod == "GET" && listReq.url?.absoluteString == "https://a-b.trycloudflare.com/api/link/sessions"
      && listReq.value(forHTTPHeaderField: "Authorization") == "Bearer " + ep.token
      && listReq.value(forHTTPHeaderField: "Origin") == nil && listReq.httpBody == nil, "the sessions request")
let task = "  Fix the flaky upload test; don't push.\n\"quoted\" $(rm -rf ~) `x`  "
let startSessionReq = WorkSessionRequests.start(ep, task: task, clientID: "ws-1")
let startSessionBody = (try? JSONSerialization.jsonObject(with: startSessionReq.httpBody ?? Data())) as? [String: String]
check(startSessionReq.httpMethod == "POST" && startSessionReq.url?.path == "/api/link/sessions/start"
      && startSessionReq.value(forHTTPHeaderField: "Content-Type") == "application/json"
      && startSessionReq.value(forHTTPHeaderField: "Authorization") == "Bearer " + ep.token
      && startSessionReq.timeoutInterval == WorkSessionRequests.startTimeout
      && startSessionBody == ["task": "Fix the flaky upload test; don't push.\n\"quoted\" $(rm -rf ~) `x`", "client_id": "ws-1"],
      "the start request: the task as typed, trimmed, in the JSON body only")
check(startSessionReq.url?.absoluteString.contains("rm") == false, "the task is never in the URL")
let longTask = WorkSessionRequests.start(ep, task: String(repeating: "t", count: 5000), clientID: String(repeating: "c", count: 99))
let longTaskBody = (try? JSONSerialization.jsonObject(with: longTask.httpBody ?? Data())) as? [String: String]
check(longTaskBody?["task"]?.count == WorkSessionRequests.maxTask && longTaskBody?["client_id"]?.count == 64,
      "a task is at most 4000 characters, a client id 64")
let detailReq = WorkSessionRequests.detail(ep, id: sid1)
check(detailReq.httpMethod == "GET" && detailReq.url?.path == "/api/link/sessions/\(sid1)", "the detail request")
let replyReq = WorkSessionRequests.reply(ep, id: sid1, text: " yes, push it \n")
check(replyReq.httpMethod == "POST" && replyReq.url?.path == "/api/link/sessions/\(sid1)/reply"
      && (try? JSONSerialization.jsonObject(with: replyReq.httpBody ?? Data())) as? [String: String] == ["text": "yes, push it"]
      && replyReq.value(forHTTPHeaderField: "Authorization") == "Bearer " + ep.token, "the reply request")
let longReply = WorkSessionRequests.reply(ep, id: sid1, text: String(repeating: "r", count: 4500))
check(((try? JSONSerialization.jsonObject(with: longReply.httpBody ?? Data())) as? [String: String])?["text"]?.count
      == WorkSessionRequests.maxReply, "a reply is at most 4000 characters")
let stopReq = WorkSessionRequests.stop(ep, id: sid1)
check(stopReq.httpMethod == "POST" && stopReq.url?.path == "/api/link/sessions/\(sid1)/stop"
      && stopReq.httpBody == Data("{}".utf8), "the stop request")
check(WorkSessionRequests.detail(ep, id: "a/../b").url?.absoluteString
      == "https://a-b.trycloudflare.com/api/link/sessions/a%2F..%2Fb", "a / in an id is escaped, never a path")

check(WorkFailure.from(status: 503, body: Data(#"{"ok": false, "error": "Work sessions are off on this Mac"}"#.utf8),
                       for: .sessions).message == "eve's Work sessions are off.", "sessions off: 503")
check(WorkFailure.from(status: 503, body: Data(#"{"ok": false, "reason": "the terminal didn't open"}"#.utf8),
                       for: .sessions).message == "the terminal didn't open", "a start that failed says why")
check(WorkFailure.from(status: 409, body: Data(#"{"ok": false, "reason": "can't reach that session's window"}"#.utf8),
                       for: .sessions).message == "can't reach that session's window"
      && WorkFailure.from(status: 409, body: Data(), for: .sessions).message == "That session has ended.",
      "a reply that can't land: 409")
check(WorkFailure.from(status: 409, body: Data(#"{"ok": false, "reason": "that session has ended", "code": "ended"}"#.utf8),
                       for: .sessions).message == WorkFailure.sessionEnded
      && WorkFailure.from(status: 409, body: Data(#"{"ok": false, "reason": "that session is still starting", "code": "x"}"#.utf8),
                          for: .sessions).message == "that session is still starting",
      "the bridge's ended 409 (code \"ended\") reads as sessionEnded, so the sheet closes its reply box")
check(WorkFailure.from(status: 404, body: Data(), for: .sessions).message == "That session has ended."
      && WorkFailure.from(status: 400, body: Data(#"{"ok": false, "reason": "the task is empty"}"#.utf8),
                          for: .sessions).message == "the task is empty", "404 and 400")
check(WorkFailure.from(status: 401, body: Data(), for: .sessions).message.hasPrefix("Eve Link's pairing is out of date")
      && WorkFailure.from(status: 530, body: Data(), for: .sessions).addressMayHaveMoved,
      "the shared words otherwise: 401 re-pair, 530 retried at the new address")
check(WorkFailure.from(status: 503, body: Data(), for: .jobs).message == "eve's chief is off.", "jobs keep their words")
print("WORK_SESSIONS_OK")
if failures > 0 { print("\(failures) check(s) failed"); exit(1) }
print("eve link: all checks passed")
