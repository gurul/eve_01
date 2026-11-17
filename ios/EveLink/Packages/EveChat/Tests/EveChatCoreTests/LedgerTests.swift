// The ledger and the line mapping, held to the web page's cases (Web/src/__tests__/media.test.ts, sources.test.ts and
// the sync cases): a streamed turn is never shown twice, a mid-turn picture lands where the transcript has it, and a
// claimed copy hands its sources to its own turn's reply.
import XCTest
@testable import EveChatCore

@MainActor
final class LedgerTests: XCTestCase {
    func testMediaLineIsItsOwnMessageEvenWithNoCaption() {
        let p = photo("")
        let msgs = ChatLines.map([line(.owner, "take a photo"), p])
        XCTAssertEqual(msgs.map(\.role), [.owner, .eve])
        XCTAssertEqual(msgs[1].id, p.id)
        XCTAssertEqual(msgs[1].text, "")
        XCTAssertEqual(msgs[1].attachments, [ChatAttachment(id: ID1, name: "001421-0.jpg", kind: .image, thumb: THUMB)])
    }

    func testMediaIsNeverMergedWithWordsBeforeOrAfter() {
        let before = line(.eve, "One sec.")
        let p = photo("The desk.")
        let after = line(.eve, "Here it is.")
        let msgs = ChatLines.map([before, p, after])
        XCTAssertEqual(msgs.map(\.text), ["One sec.", "The desk.", "Here it is."])
        XCTAssertEqual(msgs.map { $0.attachments.count }, [0, 1, 0])
    }

    func testEmptyLineIsLeftOutAndToolLinesAreHidden() {
        XCTAssertEqual(ChatLines.map([line(.eve, "  ", kind: "media")]), [])
        XCTAssertEqual(ChatLines.map([line(.eve, "ran a tool", kind: "tool")]), [])
    }

    func testCloseEveLinesMergeAndFarOnesDoNot() {
        let a = line(.eve, "One.", t: 100)
        let b = line(.eve, "Two.", t: 150)
        let c = line(.eve, "Three.", t: 400)
        let msgs = ChatLines.map([a, b, c])
        XCTAssertEqual(msgs.map(\.text), ["One.\n\nTwo.", "Three."])
        XCTAssertEqual(msgs.map(\.id), [a.id, c.id])
    }

    func testMergedLineAddsItsSourcesToTheBubbleItJoins() {
        let s1 = ChatSource(kind: .fact, text: "likes tea")
        let s2 = ChatSource(kind: .said, text: "You: no sugar", when: "Mon 9:00")
        let msgs = ChatLines.map([line(.eve, "One.", t: 100, sources: [s1]), line(.eve, "Two.", t: 110, sources: [s2, s1])])
        XCTAssertEqual(msgs.count, 1)
        XCTAssertEqual(msgs[0].sources, [s1, s2])
    }

    func testOwnerCardTapShowsItsWordsNotTheContext() {
        let sent = OwnerAction.text(human: "Yes", extra: [["size": "large"]])
        XCTAssertEqual(sent, "Yes\n\n[\"User clicked: Yes\",{\"size\":\"large\"}]")
        XCTAssertEqual(ChatLines.map([line(.owner, sent)])[0].text, "Yes")
        XCTAssertEqual(OwnerAction.display("plain words\n\n[not the context]"), "plain words\n\n[not the context]")
    }

    func testStreamedTurnIsNotShownAgainByThePoll() {
        let sink = ThreadSink()
        let ledger = Ledger(sink: sink)
        sink.thread.append(ChatMessage(id: "ask", role: .owner, text: "hi"))
        sink.idle = false
        let turn = ledger.beginTurn(sent: "hi", userID: "ask")
        turn.bytes(chunk("Hello."))
        turn.bytes(chunk(" How are you?"))
        ledger.ingest([line(.owner, "hi"), line(.eve, "Hello. How are you?")])
        turn.end()
        sink.thread.append(ChatMessage(id: "reply", role: .eve, text: "Hello. How are you?"))
        sink.idle = true
        ledger.flushIfIdle()
        XCTAssertEqual(sink.ids, ["ask", "reply"])
    }

    func testReplyLinesAreClaimedInOrderAndAnOutsideLineIsKept() {
        let sink = ThreadSink()
        let ledger = Ledger(sink: sink)
        sink.thread.append(ChatMessage(id: "ask", role: .owner, text: "plan"))
        sink.idle = false
        let turn = ledger.beginTurn(sent: "plan", userID: "ask")
        turn.bytes(chunk("First part."))
        turn.bytes(chunk("\n\nSecond part."))
        turn.end()
        sink.thread.append(ChatMessage(id: "reply", role: .eve, text: "First part.\n\nSecond part."))
        let reminder = line(.eve, "Reminder: call the dentist.")
        ledger.ingest([line(.owner, "plan"), line(.eve, "First part."), reminder, line(.eve, "Second   part.")])
        sink.idle = true
        ledger.flushIfIdle()
        XCTAssertEqual(sink.ids, ["ask", "reply", reminder.id])
    }

    func testMidTurnPictureGoesBeforeTheStreamedReplyAndIsNeverClaimed() {
        let sink = ThreadSink()
        let ledger = Ledger(sink: sink)
        sink.thread.append(ChatMessage(id: "ask", role: .owner, text: "take a photo"))
        sink.idle = false
        let turn = ledger.beginTurn(sent: "take a photo", userID: "ask")
        // The caption is the same words as the reply: still not a copy of it.
        let owner = line(.owner, "take a photo")
        let pic = photo("Here it is.")
        let said = line(.eve, "Here it is.")
        ledger.ingest([owner, pic, said])
        XCTAssertEqual(sink.ids, ["ask"])
        turn.bytes(chunk("Here it is."))
        turn.end()
        sink.thread.append(ChatMessage(id: "reply", role: .eve, text: "Here it is."))
        sink.idle = true
        ledger.flushIfIdle()
        XCTAssertEqual(sink.ids, ["ask", pic.id, "reply"])
        XCTAssertEqual(sink.thread[1].attachments.count, 1)
        // The same order as a reload reads it from history.
        XCTAssertEqual(ChatLines.map([owner, pic, said]).map(\.role), sink.thread.map(\.role))
        ledger.ingest([pic])
        XCTAssertEqual(sink.thread.count, 3)                 // the same id again is never shown twice
    }

    func testPictureAfterTheReplysOwnLineGoesAfterTheReply() {
        let sink = ThreadSink()
        let ledger = Ledger(sink: sink)
        sink.thread.append(ChatMessage(id: "ask", role: .owner, text: "hi"))
        sink.idle = false
        let turn = ledger.beginTurn(sent: "hi", userID: "ask")
        turn.bytes(chunk("Hello."))
        let task = photo("a task's screen", ID2)
        ledger.ingest([line(.owner, "hi"), line(.eve, "Hello."), task])
        turn.end()
        let late = photo("the chief's picture", ID1)
        ledger.ingest([late])                                // the stream had ended: the reply's line is written
        sink.thread.append(ChatMessage(id: "reply", role: .eve, text: "Hello."))
        sink.idle = true
        ledger.flushIfIdle()
        XCTAssertEqual(sink.ids, ["ask", "reply", task.id, late.id])
    }

    func testWithoutAPlaceBeforeTheReplyAHeldPictureIsAppended() {
        let sink = ThreadSink()
        sink.canInsert = false
        let ledger = Ledger(sink: sink)
        sink.idle = false
        let turn = ledger.beginTurn(sent: "take a photo", userID: nil)
        let pic = photo("Here it is.")
        ledger.ingest([line(.owner, "take a photo"), pic, line(.eve, "Here it is.")])
        turn.bytes(chunk("Here it is."))
        turn.end()
        sink.thread.append(ChatMessage(id: "reply", role: .eve, text: "Here it is."))
        sink.idle = true
        ledger.flushIfIdle()
        XCTAssertEqual(sink.ids, ["reply", pic.id])
    }

    func testBackToBackTurnsKeepALatePictureWithItsOwnTurn() {
        let sink = ThreadSink()
        let ledger = Ledger(sink: sink)
        // Turn A, and the poll's copy of its owner line, read while it streams.
        sink.thread.append(ChatMessage(id: "askA", role: .owner, text: "take a photo"))
        sink.idle = false
        let a = ledger.beginTurn(sent: "take a photo", userID: "askA")
        ledger.ingest([line(.owner, "take a photo")])
        a.bytes(chunk("Here it is."))
        a.end()
        sink.thread.append(ChatMessage(id: "replyA", role: .eve, text: "Here it is."))
        sink.idle = true
        ledger.flushIfIdle()
        // The queued message starts at once: turn B streams.
        sink.thread.append(ChatMessage(id: "askB", role: .owner, text: "thanks"))
        sink.idle = false
        let b = ledger.beginTurn(sent: "thanks", userID: "askB")
        // Only now does the poll bring A's last picture (before B's owner line in the transcript).
        let lateA = photo("the photo", ID1)
        ledger.ingest([lateA])
        b.bytes(chunk("Anytime."))
        // B's own picture, after B's owner line: before B's reply, as for a single turn.
        let picB = photo("a second one", ID2)
        ledger.ingest([line(.owner, "thanks"), picB])
        b.end()
        sink.thread.append(ChatMessage(id: "replyB", role: .eve, text: "Anytime."))
        sink.idle = true
        ledger.flushIfIdle()
        XCTAssertEqual(sink.ids, ["askA", "replyA", "askB", picB.id, "replyB", lateA.id])
    }

    func testPictureWithNothingStreamingShowsAtOnce() {
        let sink = ThreadSink()
        let ledger = Ledger(sink: sink)
        ledger.ingest([photo("the screen when the task ended", ID2)])
        XCTAssertEqual(sink.thread.map(\.role), [.eve])
        XCTAssertEqual(sink.thread[0].attachments[0].id, ID2)
    }

    func testClaimedCopyHandsItsSourcesToItsOwnTurnsReply() {
        let sink = ThreadSink()
        let ledger = Ledger(sink: sink)
        let found = ChatSource(kind: .remembered, text: "prefers window seats", when: "Oct 2")
        // Turn A streamed and ended; the queued turn B is streaming when the poll brings A's reply line.
        sink.thread.append(ChatMessage(id: "askA", role: .owner, text: "book it"))
        sink.idle = false
        let a = ledger.beginTurn(sent: "book it", userID: "askA")
        a.bytes(chunk("Booked."))
        a.end()
        sink.thread.append(ChatMessage(id: "replyA", role: .eve, text: "Booked."))
        sink.thread.append(ChatMessage(id: "askB", role: .owner, text: "thanks"))
        let b = ledger.beginTurn(sent: "thanks", userID: "askB")
        b.bytes(chunk("Anytime."))
        sink.thread.append(ChatMessage(id: "replyB", role: .eve, text: "Anytime."))
        ledger.ingest([line(.owner, "book it"), line(.eve, "Booked.", sources: [found])])
        b.end()
        sink.idle = true
        ledger.flushIfIdle()
        XCTAssertEqual(sink.ids, ["askA", "replyA", "askB", "replyB"])
        XCTAssertEqual(sink.thread[1].sources, [found])
        XCTAssertEqual(sink.thread[3].sources, [])
    }

    func testAnEndedClaimExpiresAfterTenMinutes() {
        var clock = 1000.0
        let sink = ThreadSink()
        let ledger = Ledger(sink: sink, now: { clock })
        let turn = ledger.beginTurn(sent: "hi", userID: nil)
        turn.bytes(chunk("Hello."))
        turn.end()
        clock += Ledger.claimTTL + 1
        let again = line(.eve, "Hello.")
        ledger.ingest([again])
        XCTAssertEqual(sink.ids, [again.id])                 // no longer a copy of anything: eve said it again
    }

    func testReplyIndexFindsTheReplyToOneOwnerMessage() {
        let thread = [
            ChatMessage(id: "a", role: .owner, text: "1"), ChatMessage(id: "ra", role: .eve, text: "r1"),
            ChatMessage(id: "b", role: .owner, text: "2"), ChatMessage(id: "pic", role: .eve, text: ""),
            ChatMessage(id: "rb", role: .eve, text: "r2"),
        ]
        XCTAssertEqual(ChatLines.replyIndex(in: thread, after: "a"), 1)
        XCTAssertEqual(ChatLines.replyIndex(in: thread, after: "b"), 4)
        XCTAssertEqual(ChatLines.replyIndex(in: thread, after: nil), 4)
        XCTAssertEqual(ChatLines.replyIndex(in: thread, after: "gone"), 4)
        XCTAssertNil(ChatLines.replyIndex(in: [ChatMessage(id: "x", role: .eve, text: "hi")], after: nil))
    }

    func testSseTextReadsContentAcrossSplitChunksAndIgnoresPings() {
        var sse = SseText()
        let event = chunk("Hello, ")
        sse.push(String(event.prefix(20)))
        XCTAssertEqual(sse.text, "")
        sse.push(String(event.dropFirst(20)))
        sse.push(": ping\n\n")
        sse.push(chunk("world."))
        sse.push("data: [DONE]\n\n")
        sse.push("data: {broken\n")
        sse.end()
        XCTAssertEqual(sse.text, "Hello, world.")
    }
}
