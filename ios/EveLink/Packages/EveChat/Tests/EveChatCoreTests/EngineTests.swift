// The engine against a scripted door (docs/app-chat.md): history, the long poll, a streamed turn, sends queued while
// eve replies, Stop, a failed turn, the hidden wait, infinite scroll, reactions and Horizontal History. The queue
// cases follow Web/src/__tests__/sendQueue.test.ts.
import XCTest
@testable import EveChatCore

/// The Mac, scripted: each call waits until the test answers it.
@MainActor
final class FakeDoor: ChatDoor {
    struct Turn {
        let id: String
        let text: String
        let clientID: String
        let attachments: [String]
        let onChunk: @MainActor (String) -> Void
        let onEnd: @MainActor (TurnEnd) -> Void
    }

    struct Ask {
        let after: String?
        let wait: Int
        let before: String?
        let answer: CheckedContinuation<Result<String, DoorFailure>, Never>
    }

    var turns: [Turn] = []
    var asks: [Ask] = []
    var topicAnswers: [Result<String, DoorFailure>] = []
    var topicCalls = 0
    var bookmarks: [(topic: String, on: Bool)] = []
    var bookmarkAnswer: Result<String, DoorFailure> = .success("{\"ok\":true}")

    func turn(id: String, text: String, clientID: String, attachments: [String],
              onChunk: @escaping @MainActor (String) -> Void, onEnd: @escaping @MainActor (TurnEnd) -> Void) {
        turns.append(Turn(id: id, text: text, clientID: clientID, attachments: attachments, onChunk: onChunk, onEnd: onEnd))
    }

    func messages(after: String?, wait: Int, before: String?) async -> Result<String, DoorFailure> {
        await withCheckedContinuation { asks.append(Ask(after: after, wait: wait, before: before, answer: $0)) }
    }

    func topics() async -> Result<String, DoorFailure> {
        topicCalls += 1
        return topicAnswers.isEmpty ? .success("{\"topics\":[],\"bookmarked\":[]}") : topicAnswers.removeFirst()
    }

    func bookmark(topic: String, on: Bool) async -> Result<String, DoorFailure> {
        bookmarks.append((topic, on))
        return bookmarkAnswer
    }

    /// Answer the oldest waiting messages request.
    func answer(_ rows: [[String: Any]], cursor: String = "c1", busy: Bool = false, more: Bool? = nil,
                reactions: [String: String]? = nil) {
        var body: [String: Any] = ["messages": rows, "cursor": cursor, "busy": busy]
        if let more { body["more"] = more }
        if let reactions { body["reactions"] = reactions }
        let json = String(decoding: try! JSONSerialization.data(withJSONObject: body), as: UTF8.self)
        asks.removeFirst().answer.resume(returning: .success(json))
    }

    func fail() { asks.removeFirst().answer.resume(returning: .failure(DoorFailure("offline"))) }
}

func row(_ id: String, _ role: String, _ text: String, t: Double = 100, extra: [String: Any] = [:]) -> [String: Any] {
    var r: [String: Any] = ["id": id, "role": role, "text": text, "t": t, "via": "app", "kind": "say"]
    for (k, v) in extra { r[k] = v }
    return r
}

@MainActor
final class EngineTests: XCTestCase {
    var door: FakeDoor!
    var engine: ChatEngine!
    var haptics: [ChatHaptic] = []

    override func setUp() async throws {
        door = FakeDoor()
        engine = ChatEngine(door: door, sleep: { _ in await Task.yield() })
        haptics = []
        engine.haptic = { [unowned self] in self.haptics.append($0) }
    }

    /// Let the engine's tasks run until `ready` holds.
    func until(_ ready: @MainActor () -> Bool, _ what: String = "", file: StaticString = #filePath, line: UInt = #line) async {
        for _ in 0..<2000 {
            if ready() { return }
            await Task.yield()
        }
        XCTFail("never happened: \(what)", file: file, line: line)
    }

    /// Start, answer the history read, and wait for the first long poll.
    func load(_ rows: [[String: Any]] = [], reactions: [String: String]? = nil) async {
        engine.start()
        await until({ self.door.asks.count == 1 }, "the history read")
        door.answer(rows, cursor: "c0", reactions: reactions)
        await until({ self.engine.historyLoaded && self.door.asks.count == 1 }, "the first poll")
    }

    func testHistoryLoadsAndTheLongPollFollowsWithItsCursor() async {
        await load([row("2026-01-05:1", "owner", "hi"), row("2026-01-05:2", "eve", "Hello.")])
        XCTAssertEqual(engine.messages.map(\.text), ["hi", "Hello."])
        XCTAssertEqual(door.asks[0].after, "c0")
        XCTAssertEqual(door.asks[0].wait, ChatEngine.idleWait)
        door.answer([row("2026-01-05:3", "eve", "A reminder.", t: 900)], cursor: "c1", busy: true)
        await until({ self.door.asks.count == 1 && self.engine.messages.count == 3 }, "the line and the next poll")
        XCTAssertEqual(door.asks[0].after, "c1")
        XCTAssertEqual(door.asks[0].wait, ChatEngine.busyWait)          // busy: a short wait
        XCTAssertTrue(engine.showsTyping)                               // a turn started elsewhere is in flight
        XCTAssertEqual(haptics, [.soft])                                // a line from eve arrived
        XCTAssertEqual(engine.arrivalTick, 1)
    }

    func testHistoryIsRetriedQuietlyUntilItComes() async {
        engine.start()
        await until({ self.door.asks.count == 1 })
        door.fail()
        await until({ self.door.asks.count == 1 }, "a second try")
        XCTAssertFalse(engine.historyLoaded)
        door.answer([row("2026-01-05:1", "eve", "Hello.")])
        await until({ self.engine.historyLoaded })
        XCTAssertEqual(engine.messages.count, 1)
    }

    func testATurnStreamsItsReplyAndThePollsCopiesAreDropped() async {
        await load()
        engine.send("  what's up  ")
        XCTAssertEqual(door.turns.count, 1)
        XCTAssertEqual(door.turns[0].text, "what's up")
        XCTAssertEqual(engine.messages.map(\.text), ["what's up"])
        XCTAssertTrue(engine.running)
        XCTAssertTrue(engine.showsTyping)                               // no words yet
        XCTAssertEqual(engine.queued, [])
        door.turns[0].onChunk(": ping\n\n")
        XCTAssertEqual(haptics, [])                                     // a keep-alive is not a reply
        door.turns[0].onChunk(chunk("Not "))
        door.turns[0].onChunk(chunk("much."))
        XCTAssertEqual(engine.messages.map(\.text), ["what's up", "Not much."])
        XCTAssertTrue(engine.messages[1].streaming)
        XCTAssertFalse(engine.showsTyping)
        XCTAssertEqual(haptics, [.soft])
        // The poll brings the turn's own lines while it streams: held, then dropped as copies.
        door.answer([row("2026-01-05:1", "owner", "what's up"), row("2026-01-05:2", "eve", "Not much.")], cursor: "c1")
        await until({ self.door.asks.count == 1 }, "the next poll")
        XCTAssertEqual(engine.messages.count, 2)
        door.turns[0].onEnd(.done)
        XCTAssertFalse(engine.running)
        XCTAssertFalse(engine.messages[1].streaming)
        XCTAssertEqual(engine.messages.map(\.text), ["what's up", "Not much."])
        XCTAssertEqual(haptics, [.soft])
    }

    func testSendsMadeWhileEveRepliesWaitInOrderAndGoOneAtATime() async {
        await load()
        engine.send("one")
        engine.send("two")
        engine.send("three", attachments: [ChatAttachment(id: ID1, name: "a.jpg", kind: .image)])
        XCTAssertEqual(door.turns.map(\.text), ["one"])
        XCTAssertEqual(engine.queued.map(\.text), ["two", "three"])
        XCTAssertEqual(engine.messages.map(\.text), ["one"])            // a queued send is not in the thread yet
        door.turns[0].onChunk(chunk("First."))
        door.turns[0].onEnd(.done)
        XCTAssertEqual(door.turns.map(\.text), ["one", "two"])          // the next one went at once
        XCTAssertEqual(engine.queued.map(\.text), ["three"])
        XCTAssertEqual(engine.messages.map(\.text), ["one", "First.", "two"])
        door.turns[1].onEnd(.failed("offline"))                         // a failed reply lets the next one go
        XCTAssertEqual(door.turns.map(\.text), ["one", "two", "three"])
        XCTAssertEqual(door.turns[2].attachments, [ID1])
        XCTAssertEqual(engine.queued, [])
        XCTAssertEqual(engine.messages.filter { $0.role == .owner }.map(\.text), ["one", "two", "three"])
        XCTAssertEqual(engine.problems.count, 1)                        // "two" did not go, and says why
        XCTAssertEqual(engine.problems[engine.messages[2].id], "offline")
    }

    func testStopEndsTheReplyAndTheRestComesAfterTheQueuedTurnsReply() async {
        await load()
        engine.send("long question")
        door.turns[0].onChunk(chunk("The first half"))
        engine.send("next")
        door.turns[0].onEnd(.done)                                      // Stop: the phone stops reading
        XCTAssertEqual(door.turns.map(\.text), ["long question", "next"])
        door.turns[1].onChunk(chunk("Sure."))
        // eve finished the stopped reply on the Mac first: its whole line arrives while the next turn streams.
        door.answer([row("2026-01-05:1", "owner", "long question"),
                     row("2026-01-05:2", "eve", "The first half and the second."),
                     row("2026-01-05:3", "owner", "next")], cursor: "c1")
        await until({ self.door.asks.count == 1 })
        XCTAssertEqual(engine.messages.map(\.text), ["long question", "The first half", "next", "Sure."])
        door.turns[1].onEnd(.done)
        XCTAssertEqual(engine.messages.map(\.text),
                       ["long question", "The first half", "next", "Sure.", "The first half and the second."])
    }

    func testASendWaitsWhileHiddenAfterACutStreamAndGoesWhenTheChatShows() async {
        await load()
        engine.send("one")
        engine.send("two")
        engine.setVisible(false)
        door.turns[0].onEnd(.interrupted)                               // the app stopped reading in the background
        XCTAssertEqual(door.turns.count, 1)
        XCTAssertEqual(engine.queued.map(\.text), ["two"])
        XCTAssertEqual(engine.problems, [:])                            // eve is still answering on the Mac
        engine.setVisible(true)
        XCTAssertEqual(door.turns.map(\.text), ["one", "two"])
        XCTAssertEqual(engine.queued, [])
    }

    func testSendsWaitForTheHistory() async {
        engine.start()
        await until({ self.door.asks.count == 1 })
        engine.send("early")
        XCTAssertEqual(door.turns.count, 0)
        XCTAssertEqual(engine.queued.map(\.text), ["early"])
        door.answer([row("2026-01-05:1", "eve", "Hello.")])
        await until({ self.door.turns.count == 1 })
        XCTAssertEqual(engine.messages.map(\.text), ["Hello.", "early"])
    }

    func testACardTapGoesWithItsContextAndIsRefusedWhileEveReplies() async {
        await load()
        XCTAssertEqual(engine.ask("Book it", data: [["date": "Friday"]]), "")
        XCTAssertEqual(door.turns[0].text, "Book it\n\n[\"User clicked: Book it\",{\"date\":\"Friday\"}]")
        XCTAssertEqual(engine.messages.last?.text, "Book it")
        XCTAssertEqual(engine.ask("Again"), ChatEngine.busyLine)
        XCTAssertEqual(door.turns.count, 1)
        // The poll's copy of the owner line carries the context; it is still this turn's own line.
        door.answer([row("2026-01-05:1", "owner", door.turns[0].text)], cursor: "c1")
        await until({ self.door.asks.count == 1 })
        door.turns[0].onEnd(.done)
        XCTAssertEqual(engine.messages.map(\.text), ["Book it"])
    }

    func testAMidTurnPictureLandsBeforeTheStreamedReply() async {
        await load()
        engine.send("take a photo")
        door.answer([row("2026-01-05:1", "owner", "take a photo"),
                     row("2026-01-05:2", "eve", "", extra: ["kind": "media", "attachments":
                        [["id": ID1, "name": "desk.jpg", "kind": "image", "thumb": THUMB]]])], cursor: "c1")
        await until({ self.door.asks.count == 1 })
        door.turns[0].onChunk(chunk("Here it is."))
        door.turns[0].onEnd(.done)
        XCTAssertEqual(engine.messages.map(\.text), ["take a photo", "", "Here it is."])
        XCTAssertEqual(engine.messages[1].attachments.map(\.id), [ID1])
    }

    func testReactionsReachTheBubbleThatSentTheTurnAndHistoryRows() async {
        await load([row("2026-01-05:1", "owner", "star this", extra: ["rid": "turn-0001"])],
                   reactions: ["turn-0001": "🏆"])
        XCTAssertEqual(engine.reaction(for: "2026-01-05:1"), "🏆")
        engine.send("note this")
        let rid = door.turns[0].clientID
        XCTAssertEqual(engine.reaction(for: engine.messages[1].id), "")
        door.answer([], cursor: "c1", reactions: [rid: "✍️", "turn-0001": "🏆"])
        await until({ self.engine.reaction(for: self.engine.messages[1].id) == "✍️" }, "the reaction")
        door.answer([], cursor: "c2", reactions: [rid: "✍️"])            // one was taken off
        await until({ self.engine.reaction(for: "2026-01-05:1") == "" }, "the reaction gone")
    }

    func testAnOlderPageGoesAboveOnceAndEndsWhenThereIsNoMore() async {
        await load([row("2026-01-05:7", "eve", "Newest.")])
        engine.loadOlder()
        await until({ self.door.asks.count == 2 }, "the older page's request")
        let older = door.asks.firstIndex { $0.before != nil }!
        XCTAssertEqual(door.asks[older].before, "2026-01-05:7")
        XCTAssertNil(door.asks[older].after)
        engine.loadOlder()                                              // one at a time
        XCTAssertEqual(door.asks.count, 2)
        let ask = door.asks.remove(at: older)
        let body: [String: Any] = ["messages": [row("2026-01-04:3", "owner", "Older."), row("2026-01-04:4", "eve", "Yes.")],
                                   "more": false, "cursor": "", "busy": false]
        ask.answer.resume(returning: .success(String(decoding: try! JSONSerialization.data(withJSONObject: body), as: UTF8.self)))
        await until({ self.engine.messages.count == 3 })
        XCTAssertEqual(engine.messages.map(\.text), ["Older.", "Yes.", "Newest."])
        XCTAssertEqual(engine.olderTick, 1)
        XCTAssertFalse(engine.hasOlder)
        engine.loadOlder()
        XCTAssertEqual(door.asks.count, 1)                              // the first day was reached: nothing asked
        XCTAssertEqual(door.asks[0].after, "c0")                        // the live poll's cursor never moved
    }

    func testHistoryCardsOpenJumpAndStar() async {
        let topic: [String: Any] = ["id": "2026-01-05:1", "first_id": "2026-01-05:1", "last_id": "2026-01-05:9",
                                    "started_at": 1, "ended_at": 2, "count": 4, "title": "Trip plans",
                                    "preview": "Two options.", "bookmarked": false]
        let away: [String: Any] = ["id": "2025-12-01:1", "first_id": "2025-12-01:1", "last_id": "2025-12-01:5",
                                   "title": "Old thing", "count": 2]
        let json = String(decoding: try! JSONSerialization.data(withJSONObject: ["topics": [topic, away], "bookmarked": []]),
                          as: UTF8.self)
        door.topicAnswers = [.success(json), .success(json)]
        await load([row("2026-01-05:2", "owner", "plan a trip"), row("2026-01-05:3", "eve", "Two options.")])
        await until({ self.engine.topics.topics.count == 2 }, "the topics")
        engine.toggleHistory()
        XCTAssertTrue(engine.historyShown)
        let loaded = engine.topics.topics[0]
        engine.open(loaded)
        XCTAssertFalse(engine.historyShown)
        XCTAssertEqual(engine.jump?.id, "2026-01-05:2")                 // its first loaded message
        XCTAssertEqual(door.turns.count, 0)
        engine.open(engine.topics.topics[1])                            // none of it is loaded: pick it back up
        XCTAssertEqual(door.turns.map(\.text), ["Let's pick up \"Old thing\" again."])

        door.bookmarkAnswer = .failure(DoorFailure("no"))
        engine.star(loaded)
        XCTAssertEqual(engine.topics.bookmarked.map(\.id), ["2026-01-05:1"])   // shown at once
        await until({ self.engine.topics.bookmarked.isEmpty }, "the star put back")
        XCTAssertEqual(engine.historyNote, "Couldn't save that topic. Try again.")
        XCTAssertEqual(door.bookmarks.map(\.on), [true])
    }

    func testStartingAgainClearsTheScreenAndIgnoresTheOldRunsAnswers() async {
        await load([row("2026-01-05:1", "eve", "Hello.")])
        engine.send("one")
        let old = door.turns[0]
        engine.start()                                                  // a new pairing
        XCTAssertEqual(engine.messages, [])
        XCTAssertFalse(engine.running)
        old.onChunk(chunk("late"))
        old.onEnd(.done)
        XCTAssertEqual(engine.messages, [])
    }
}
