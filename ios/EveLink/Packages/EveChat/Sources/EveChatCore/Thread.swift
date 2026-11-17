// The chat's messages, and the ledger that keeps the live long poll from showing a turn twice (docs/app-chat.md).
//
// eve has ONE conversation. History comes from GET /api/link/messages; afterwards a long poll brings lines eve
// sends on its own (reminders, questions, replies to a Telegram or voice turn). A turn started in the app streams its
// reply through /api/link/turn, and the same lines also reach the transcript, so the poll sees them again. The ledger
// drops those second copies:
//   - the owner line of an app turn matches the text the turn sent;
//   - eve lines match, in order, the text the turn streamed.
// While a turn is still streaming, lines from the poll are held and judged once the turn has settled, so an outside
// line never lands in the middle of a reply.
//
// A picture or file eve sends mid-turn (a photo, a screenshot, a file) is a "media" line with attachments. The
// stream carries words only, so it is never claimed as a copy: it is held like any other line and, once the turn
// settles, goes in just before the streamed reply, as its own message, where the transcript has it. One that came in
// after the reply's own line, or once the stream ended, is after the reply in the transcript too, and is appended.
//
// A reply that leaned on eve's memory carries sources. A merged line adds its own to the bubble it joins, and a
// claimed copy of a streamed reply hands its sources to that reply, since its words are already shown.
//
// Turns can run back to back: a message the owner queued while eve replied starts the moment the reply before it
// ends (SendQueue.swift). The poll may then bring the earlier turn's last lines while the next one streams. They stay
// with their own turn: a claimed copy names the owner message its turn sent, so its sources go on that turn's reply,
// and a picture that follows an ended turn's owner line is appended, never put before the reply streaming now.
import Foundation

/// One bubble in the chat.
public struct ChatMessage: Identifiable, Equatable, Sendable {
    public enum Role: Sendable { case owner, eve }

    /// The transcript's id ("2026-11-17:42") for a line read from the Mac; a local id for a message this app sent
    /// or a reply it streamed.
    public var id: String
    public var role: Role
    /// What the bubble shows: the owner's words, or eve's reply as markdown with any cards.
    public var text: String
    public var attachments: [ChatAttachment]
    /// The memory lines an eve reply leaned on.
    public var sources: [ChatSource]
    /// The reply is still arriving.
    public var streaming: Bool

    public init(id: String, role: Role, text: String, attachments: [ChatAttachment] = [], sources: [ChatSource] = [],
                streaming: Bool = false) {
        self.id = id
        self.role = role
        self.text = text
        self.attachments = attachments
        self.sources = sources
        self.streaming = streaming
    }

    /// A transcript id: `<day>:<line index>`. Only these can be asked for an older page, or be a topic's start.
    public static func isTranscriptID(_ id: String) -> Bool {
        id.range(of: #"^\d{4}-\d{2}-\d{2}:\d+$"#, options: .regularExpression) != nil
    }
}

public enum ChatLines {
    /// Consecutive eve lines closer than this become one bubble in history.
    public static let mergeWindow: Double = 120

    /// Whitespace runs as one space, ends trimmed: how two copies of the same words are compared.
    public static func norm(_ text: String) -> String {
        text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    static func message(_ w: WireMessage) -> ChatMessage {
        switch w.role {
        case .eve:
            ChatMessage(id: w.id, role: .eve, text: w.text, attachments: w.attachments, sources: w.sources)
        case .owner:
            ChatMessage(id: w.id, role: .owner, text: OwnerAction.display(w.text), attachments: w.attachments)
        }
    }

    /// Transcript lines as bubbles. Consecutive eve lines within `mergeWindow` of each other merge into one (joined
    /// by a blank line, as a streamed turn is); further apart they stay separate. A line with a picture or file is
    /// always its own message, never merged into or onto: its attachments are its own.
    public static func map(_ lines: [WireMessage]) -> [ChatMessage] {
        var out: [ChatMessage] = []
        var last: (t: Double, media: Bool)?
        for w in lines where w.visible {
            let media = !w.attachments.isEmpty
            if w.role == .eve, !media, let l = last, !l.media, out.last?.role == .eve, w.t - l.t <= mergeWindow {
                out[out.count - 1].text += "\n\n" + w.text
                out[out.count - 1].sources = ChatSource.merged(out[out.count - 1].sources + w.sources)
                last = (w.t, false)
                continue
            }
            out.append(message(w))
            last = (w.t, media)
        }
        return out
    }

    /// The reply to owner message `userID`: the last eve message after it and before the next owner message.
    /// Without `userID`, or when it is not in the thread, the reply to the last owner message.
    public static func replyIndex(in thread: [ChatMessage], after userID: String?) -> Int? {
        let own = userID.flatMap { id in thread.firstIndex { $0.id == id && $0.role == .owner } }
        guard let from = own ?? thread.lastIndex(where: { $0.role == .owner }) else { return nil }
        let next = thread[(from + 1)...].firstIndex { $0.role == .owner } ?? thread.endIndex
        return thread[(from + 1)..<next].lastIndex { $0.role == .eve }
    }
}

/// Pulls `choices[0].delta.content` out of the reply stream (OpenAI Chat Completions server-sent events).
public struct SseText: Sendable {
    private var buffer = ""
    public private(set) var text = ""

    public init() {}

    public mutating func push(_ chunk: String) {
        buffer += chunk
        while let nl = buffer.firstIndex(of: "\n") {
            line(buffer[..<nl])
            buffer.removeSubrange(...nl)
        }
    }

    public mutating func end() {
        if !buffer.isEmpty { line(buffer[...]) }
        buffer = ""
    }

    private mutating func line(_ raw: Substring) {
        guard raw.hasPrefix("data:") else { return }             // a ": ping" keep-alive adds nothing
        let data = raw.dropFirst(5).trimmingCharacters(in: .whitespaces)
        guard !data.isEmpty, data != "[DONE]",
              let json = try? JSONSerialization.jsonObject(with: Data(data.utf8)) as? [String: Any],
              let choice = (json["choices"] as? [Any])?.first as? [String: Any],
              let content = (choice["delta"] as? [String: Any])?["content"] as? String else { return }
        text += content
    }
}

/// What the ledger does to the thread. The engine implements it over its own list.
@MainActor
public protocol LedgerSink: AnyObject {
    /// Put messages at the end of the thread.
    func append(_ messages: [ChatMessage])
    /// No turn of the app's own is in flight or settling.
    var idle: Bool { get }
    /// Put messages just before the app's own streamed reply: the thread's last message, when it is eve's and an
    /// owner message is before it. False, with nothing changed, when there is no such reply.
    func insertBeforeReply(_ messages: [ChatMessage]) -> Bool
    /// The poll's copy of the app's own streamed reply carried sources: put them on the reply to `userID`.
    func attachSources(_ sources: [ChatSource], userID: String?)
}

@MainActor
public final class Ledger {
    /// How long an app turn can still claim lines from the poll, in seconds.
    public static let claimTTL: Double = 10 * 60

    /// One app turn: what it sent and what it has streamed.
    public final class Claim {
        let sentText: String
        /// The owner message this turn sent: its reply is the one after it.
        let userID: String?
        var ownerClaimed = false
        var reply = SseText()
        /// How far into the normalized reply the claimed copies reach, in characters. A count, not an index: the
        /// reply is normalized again on every look, and an index into one string is not valid in the next.
        var consumed = 0
        public private(set) var ended = false
        let startedAt: Double

        init(sentText: String, userID: String?, startedAt: Double) {
            self.sentText = sentText
            self.userID = userID
            self.startedAt = startedAt
        }

        /// The stream's text, as it arrives.
        public func bytes(_ text: String) { reply.push(text) }

        /// The stream is over. Ended, not flushed: the held lines wait for `flushIfIdle`.
        public func end() {
            guard !ended else { return }
            ended = true
            reply.end()
        }

        /// The reply so far (a keep-alive adds none).
        public var text: String { reply.text }
    }

    private weak var sink: LedgerSink?
    private let now: () -> Double
    private var seen = Set<String>()
    private var claims: [Claim] = []
    /// The turn the poll is reading: the claim the last owner line it brought matched.
    private var reading: Claim?
    private var held: [WireMessage] = []
    /// Held media lines that came in while a reply was streaming, before its own line: they go before the streamed
    /// reply, not after it.
    private var beforeReply = Set<String>()

    public init(sink: LedgerSink, now: @escaping () -> Double = { Date().timeIntervalSince1970 }) {
        self.sink = sink
        self.now = now
    }

    /// Ids already on screen (the history load, an older page).
    public func markSeen(_ lines: [WireMessage]) {
        for w in lines { seen.insert(w.id) }
    }

    /// An app turn started (`userID`: the owner message it sent).
    public func beginTurn(sent: String, userID: String?) -> Claim {
        let claim = Claim(sentText: ChatLines.norm(sent), userID: userID, startedAt: now())
        claims.append(claim)
        return claim
    }

    public var streaming: Bool { claims.contains { !$0.ended } }

    /// New lines from the long poll, oldest first.
    public func ingest(_ lines: [WireMessage]) {
        expire()
        var fresh: [WireMessage] = []
        // While a reply streams, its own transcript line is written at the turn's end: a media line before any eve
        // words in this batch precedes it.
        var before = streaming
        for w in lines {
            if seen.contains(w.id) { continue }
            seen.insert(w.id)
            guard w.visible else { continue }
            if w.role == .owner {
                reading = claimOwner(w)
                if reading != nil { continue }
            }
            let media = !w.attachments.isEmpty
            if w.role == .eve, !media { before = false }
            // A picture of a turn that has ended belongs to that turn, not before the reply streaming now.
            if media, before, reading?.ended != true { beforeReply.insert(w.id) }
            fresh.append(w)
        }
        if !isIdle || !held.isEmpty {
            // Hold everything until the turn settles, so an outside line never lands in the middle of a reply.
            held.append(contentsOf: fresh)
            flushIfIdle()
            return
        }
        emit(fresh)
    }

    /// Release the lines held during a turn. Call once the turn has settled, so the streamed reply is already in the
    /// thread and an outside line lands after it.
    public func flushIfIdle() {
        guard isIdle, !held.isEmpty else { return }
        let lines = held
        held = []
        emit(lines)
    }

    private var isIdle: Bool { !streaming && (sink?.idle ?? true) }

    private func emit(_ lines: [WireMessage]) {
        var keep = lines.filter { !($0.role == .eve && claimReply($0)) }
        let early = keep.filter { beforeReply.contains($0.id) }
        for w in lines { beforeReply.remove(w.id) }
        if !early.isEmpty, sink?.insertBeforeReply(ChatLines.map(early)) == true {
            let ids = Set(early.map(\.id))
            keep.removeAll { ids.contains($0.id) }
        }
        let messages = ChatLines.map(keep)
        if !messages.isEmpty { sink?.append(messages) }
    }

    private func claimOwner(_ w: WireMessage) -> Claim? {
        let text = ChatLines.norm(w.text)
        guard let claim = claims.first(where: { !$0.ownerClaimed && $0.sentText == text }) else { return nil }
        claim.ownerClaimed = true
        return claim
    }

    private func claimReply(_ w: WireMessage) -> Bool {
        // A picture or file never came through the stream, so its line is never a copy of one, whatever its caption.
        guard w.attachments.isEmpty else { return false }
        let text = ChatLines.norm(w.text)
        guard !text.isEmpty else { return false }
        for claim in claims {
            let reply = ChatLines.norm(claim.reply.text)
            let from = reply.index(reply.startIndex, offsetBy: min(claim.consumed, reply.count))
            if let found = reply.range(of: text, range: from..<reply.endIndex) {
                claim.consumed = reply.distance(from: reply.startIndex, to: found.upperBound)
                // The words are on screen already; what memory they leaned on is not.
                if !w.sources.isEmpty { sink?.attachSources(w.sources, userID: claim.userID) }
                return true
            }
        }
        return false
    }

    private func expire() {
        let cutoff = now() - Self.claimTTL
        claims.removeAll { $0.ended && $0.startedAt < cutoff }
    }
}
