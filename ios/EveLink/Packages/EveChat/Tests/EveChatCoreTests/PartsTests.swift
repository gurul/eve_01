// How a reply splits, what the Mac's JSON may hold, the send queue and the topic cards. The cases follow
// Web/src/__tests__/replyParts.test.ts, attachments.test.ts, sources.test.ts, reactions.test.ts, sendQueue.test.ts
// and history.test.ts.
import XCTest
@testable import EveChatCore

final class PartsTests: XCTestCase {
    let block = "{\"op\":\"add\",\"path\":\"/root\",\"value\":\"card\"}"

    func testPlainMarkdownIsOnePart() {
        XCTAssertEqual(ReplyParts.split("Hello **there**.", streaming: false), [.markdown("Hello **there**.")])
        XCTAssertEqual(ReplyParts.split("", streaming: false), [])
        XCTAssertEqual(ReplyParts.split("  \n ", streaming: true), [])
    }

    func testACardBetweenProse() {
        let reply = "Here you go.\n\n```json-render\n\(block)\n```\n\nAnything else?"
        XCTAssertEqual(ReplyParts.split(reply, streaming: false),
                       [.markdown("Here you go."), .card(code: block, complete: true), .markdown("Anything else?")])
    }

    func testAStreamingCardIsIncompleteUntilItsFenceCloses() {
        XCTAssertEqual(ReplyParts.split("Look:\n```json-render", streaming: true),
                       [.markdown("Look:"), .card(code: "", complete: false)])
        XCTAssertEqual(ReplyParts.split("Look:\n```json-render\n\(block)\n{\"op\":\"ad", streaming: true),
                       [.markdown("Look:"), .card(code: "\(block)\n{\"op\":\"ad", complete: false)])
        XCTAssertEqual(ReplyParts.split("```json-render\n\(block)\n```", streaming: true), [.card(code: block, complete: true)])
    }

    func testAHalfTypedFenceOpenerIsHeldBackWhileStreaming() {
        XCTAssertEqual(ReplyParts.split("Here it is.\n```json", streaming: true), [.markdown("Here it is.")])
        XCTAssertEqual(ReplyParts.split("Here it is.\n```json", streaming: false), [.markdown("Here it is.\n```json")])
        XCTAssertEqual(ReplyParts.split("Use `code` here", streaming: true), [.markdown("Use `code` here")])
    }

    func testTwoCardsAndAnEmptyOne() {
        let reply = "```json-render\n\(block)\n```\nand\n```json-render\n```\n"
        XCTAssertEqual(ReplyParts.split(reply, streaming: false),
                       [.card(code: block, complete: true), .markdown("and"), .card(code: "", complete: true)])
    }

    func testACardInTheOlderFormatIsANoteNeverCode() {
        let fenced = "Compare:\n```openui-lang\nroot = Card([t])\nt = TextContent(\"x\")\n```\nDone."
        XCTAssertEqual(ReplyParts.split(fenced, streaming: false), [.markdown("Compare:"), .legacyCard, .markdown("Done.")])
        XCTAssertEqual(ReplyParts.split("Sure.\nroot = Card([a])\na = TextContent(\"x\")", streaming: false),
                       [.markdown("Sure."), .legacyCard])
        XCTAssertEqual(ReplyParts.split("The root = 5 in that sum.", streaming: false), [.markdown("The root = 5 in that sum.")])
        // An ordinary code fence is markdown, whatever it holds.
        let code = "```swift\nroot = Card(x)\n```"
        XCTAssertEqual(ReplyParts.split(code, streaming: false), [.markdown(code)])
    }

    func testAttachmentsAreSanitized() {
        let good: [String: Any] = ["id": ID1, "name": "desk.jpg", "kind": "image", "thumb": THUMB]
        let raw: [Any] = [
            good, ["id": "../etc/passwd", "name": "x", "kind": "pdf"], ["id": ID2, "name": "x", "kind": "script"],
            ["id": ID2, "name": "  ", "kind": "pdf"], "junk", ["id": ID2, "kind": "image", "thumb": "https://example.com/a.jpg"],
            ["id": ID2, "kind": "image", "gone": true, "thumb": THUMB], good, good,
        ]
        let out = ChatAttachment.sanitize(raw)
        XCTAssertEqual(out.count, ChatAttachment.maxCount)
        XCTAssertEqual(out[0], ChatAttachment(id: ID1, name: "desk.jpg", kind: .image, thumb: THUMB))
        XCTAssertEqual(out[1], ChatAttachment(id: ID2, name: "File", kind: .pdf))
        XCTAssertEqual(out[2], ChatAttachment(id: ID2, name: "Photo", kind: .image))             // a thumb is never a URL
        XCTAssertEqual(out[3], ChatAttachment(id: ID2, name: "Photo", kind: .image, gone: true))  // expired: no picture
        XCTAssertEqual(ChatAttachment.sanitize("nope"), [])
        XCTAssertNotNil(out[0].thumbData)
        XCTAssertFalse(ChatAttachment.isThumb("data:image/svg+xml;base64,AAAA"))
        XCTAssertFalse(ChatAttachment.isThumb("data:image/png;base64,AA=A"))
        XCTAssertEqual(ChatAttachment.sanitize([["id": ID1, "name": "photos.zip", "kind": "file"]])[0].kind, .file)
    }

    func testSourcesAreSanitized() {
        let long = String(repeating: "word ", count: 80)
        let raw: [Any] = [
            ["kind": "fact", "text": "  likes   tea ", "when": "Oct 2"], ["kind": "fact", "text": "likes tea"],
            ["kind": "guess", "text": "x"], ["kind": "said", "text": 5], ["kind": "said", "text": long, "when": 7],
            ["kind": "remembered", "text": ""],
        ]
        let out = ChatSource.sanitize(raw)
        XCTAssertEqual(out.count, 2)
        XCTAssertEqual(out[0], ChatSource(kind: .fact, text: "likes tea", when: "Oct 2"))
        XCTAssertEqual(out[1].text.count, ChatSource.maxChars)
        XCTAssertTrue(out[1].text.hasSuffix("…"))
        XCTAssertNil(out[1].when)
        let many = (0..<10).map { ["kind": "said", "text": "line \($0)"] }
        XCTAssertEqual(ChatSource.sanitize(many).count, ChatSource.maxCount)
        XCTAssertEqual(ChatSource.Kind.remembered.label, "Remembered")
    }

    func testOnlyAShortEmojiIsAReaction() {
        for ok in ["👀", "✍", "✍️", "🏆", "👍", "👍🏽", "👩‍💻"] { XCTAssertEqual(ChatReactions.sanitize(ok), ok, ok) }
        for bad in ["", "ok", "👍 ", "<b>", "1", "#", "👍👍👍👍👍👍👍👍👍", "🇺🇸", "a👍"] {
            XCTAssertEqual(ChatReactions.sanitize(bad), "", bad)
        }
        XCTAssertEqual(ChatReactions.sanitize(5), "")
        XCTAssertTrue(ChatReactions.isRid("turn-0001"))
        XCTAssertTrue(ChatReactions.isRid("4e0c8a3e-9d0b-4f6c-8f5d-2f1f6e7a9b10"))
        XCTAssertFalse(ChatReactions.isRid("short"))
        XCTAssertFalse(ChatReactions.isRid("has space 1"))
    }

    func testAMessagesAnswerIsReadLeniently() {
        let json = """
        {"messages":[{"id":"2026-01-05:1","role":"owner","text":"hi","t":12.5,"via":"app","kind":"say","rid":"turn-0001",
        "reaction":"👍","sources":[{"kind":"fact","text":"never on an owner row"}]},
        {"id":"2026-01-05:2","role":"eve","text":"Hello.","sources":[{"kind":"fact","text":"likes tea"}],"rid":"turn-0002"},
        {"id":"2026-01-05:3","role":"system","text":"x"},{"role":"eve","text":"no id"},7],
        "cursor":"abc~3","busy":true,"reactions":{"turn-0001":"👍","bad rid":"👍","turn-0003":"nope"},"more":true}
        """
        let p = MessagesPayload.parse(json)!
        XCTAssertEqual(p.messages.map(\.id), ["2026-01-05:1", "2026-01-05:2"])
        XCTAssertEqual(p.messages[0].rid, "turn-0001")
        XCTAssertEqual(p.messages[0].reaction, "👍")
        XCTAssertEqual(p.messages[0].sources, [])
        XCTAssertEqual(p.messages[0].t, 12.5)
        XCTAssertNil(p.messages[1].rid)
        XCTAssertEqual(p.messages[1].sources, [ChatSource(kind: .fact, text: "likes tea")])
        XCTAssertEqual(p.cursor, "abc~3")
        XCTAssertTrue(p.busy)
        XCTAssertEqual(p.reactions, ["turn-0001": "👍"])
        XCTAssertEqual(p.more, true)
        XCTAssertNil(MessagesPayload.parse("{\"cursor\":\"x\"}"))
        XCTAssertNil(MessagesPayload.parse("not json"))
        XCTAssertNil(MessagesPayload.parse("{\"messages\":[]}")!.reactions)
    }

    func testTheQueueKeepsOrderAndTheGate() {
        var q = SendQueue()
        let open = StartGate(inFlight: false, historyLoaded: true, hiddenAfterError: false)
        XCTAssertNil(q.take(open))
        let a = q.push("one")
        let b = q.push("two", attachments: [ChatAttachment(id: ID1, name: "a.pdf", kind: .pdf)])
        XCTAssertNotEqual(a.id, b.id)
        XCTAssertNil(q.take(StartGate(inFlight: true, historyLoaded: true, hiddenAfterError: false)))
        XCTAssertNil(q.take(StartGate(inFlight: false, historyLoaded: false, hiddenAfterError: false)))
        XCTAssertNil(q.take(StartGate(inFlight: false, historyLoaded: true, hiddenAfterError: true)))
        XCTAssertEqual(q.items.map(\.text), ["one", "two"])
        XCTAssertEqual(q.take(open)?.text, "one")
        XCTAssertEqual(q.take(open)?.attachments.count, 1)
        XCTAssertNil(q.take(open))
        XCTAssertEqual(q.push("three").id, "queued:3")                  // a key is never used twice
    }

    func testTopicsAreParsedOrderedAndDated() {
        let json = """
        {"topics":[{"id":"2026-01-05:1","first_id":"2026-01-05:1","last_id":"2026-01-06:4","started_at":10,"ended_at":20,
        "count":3.4,"title":"Trip plans","preview":"Two options.","bookmarked":true},
        {"id":"2026-01-02:9","first_id":"2026-01-02:9","last_id":"2026-01-02:12","title":"Dinner","count":0},
        {"id":"bad","first_id":"2026-01-02:9","last_id":"2026-01-02:12","title":"x"},
        {"id":"2026-01-01:1","first_id":"2026-01-01:1","last_id":"2026-01-01:2","title":""}],
        "bookmarked":[{"id":"2026-01-05:1","first_id":"2026-01-05:1","last_id":"2026-01-06:4","title":"Trip plans","bookmarked":true}]}
        """
        let view = ChatTopics.parse(json)
        XCTAssertEqual(view.topics.map(\.title), ["Trip plans", "Dinner"])
        XCTAssertEqual(view.topics[0].count, 3)
        XCTAssertEqual(view.topics[1].count, 1)
        XCTAssertEqual(view.cards.map(\.id), ["2026-01-05:1", "2026-01-02:9"])           // saved first, each once
        let trip = view.topics[0]
        XCTAssertTrue(trip.contains("2026-01-05:1"))
        XCTAssertTrue(trip.contains("2026-01-05:900"))
        XCTAssertTrue(trip.contains("2026-01-06:4"))
        XCTAssertFalse(trip.contains("2026-01-06:5"))
        XCTAssertFalse(trip.contains("2026-01-04:99"))
        XCTAssertFalse(trip.contains("local:abc"))
        XCTAssertEqual(trip.start(in: ["2026-01-04:1", "local:x", "2026-01-05:7", "2026-01-05:8"]), "2026-01-05:7")
        XCTAssertNil(trip.start(in: ["2026-01-04:1"]))
        XCTAssertEqual(trip.pickUp, "Let's pick up \"Trip plans\" again.")

        let unstarred = view.withBookmark("2026-01-05:1", on: false)
        XCTAssertEqual(unstarred.bookmarked, [])
        XCTAssertFalse(unstarred.topics[0].bookmarked)
        let starred = view.withBookmark("2026-01-02:9", on: true)
        XCTAssertEqual(starred.bookmarked.map(\.id), ["2026-01-02:9", "2026-01-05:1"])
        XCTAssertEqual(ChatTopics.parse("junk"), ChatTopics())

        XCTAssertEqual(ChatTopics.when(1000, now: 1060), "now")
        XCTAssertEqual(ChatTopics.when(1000, now: 1000 + 12 * 60), "12 min")
        XCTAssertEqual(ChatTopics.when(1000, now: 1000 + 3 * 3600), "3 h")
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = TimeZone(identifier: "UTC")!
        let at = 1_767_600_000.0                                          // Mon, 5 Jan 2026 08:00 UTC
        XCTAssertEqual(ChatTopics.when(at, now: at + 2 * 86400, calendar: utc, locale: Locale(identifier: "en_US")), "Mon")
        XCTAssertEqual(ChatTopics.when(at, now: at + 30 * 86400, calendar: utc, locale: Locale(identifier: "en_US")), "Jan 5")
    }
}
