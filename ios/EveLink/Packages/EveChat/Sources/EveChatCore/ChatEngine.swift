// The chat's state and what moves it (docs/app-chat.md): the history, the live long poll, the turns the owner sends
// and their streamed replies, the queue of sends made while eve replies, infinite scroll, reactions, and Horizontal
// History. It reaches the Mac only through a `ChatDoor`, which the app implements over its native transport, so all
// of this runs in tests on the Mac with a scripted door.
import Foundation
import Observation

/// How a turn's stream ended.
public enum TurnEnd: Equatable, Sendable {
    /// The reply finished, or the owner tapped Stop.
    case done
    /// The turn did not go, or broke off: the daemon's or the network's line.
    case failed(String)
    /// The app stopped reading (25 s in the background). eve finishes the reply on the Mac and the history has it.
    case interrupted
}

public struct DoorFailure: Error, Equatable, Sendable {
    public var message: String
    public init(_ message: String) { self.message = message }
}

/// The chat's way to the Mac. Every call is answered on the main actor.
@MainActor
public protocol ChatDoor: AnyObject {
    /// POST /api/link/turn. `onChunk` hears the stream's text in order; `onEnd` is called once.
    func turn(id: String, text: String, clientID: String, attachments: [String],
              onChunk: @escaping @MainActor (String) -> Void, onEnd: @escaping @MainActor (TurnEnd) -> Void)
    /// GET /api/link/messages: the daemon's JSON. `before` asks for an older page, and is never sent with `after`.
    func messages(after: String?, wait: Int, before: String?) async -> Result<String, DoorFailure>
    /// GET /api/link/topics.
    func topics() async -> Result<String, DoorFailure>
    /// POST /api/link/topics/bookmark.
    func bookmark(topic: String, on: Bool) async -> Result<String, DoorFailure>
}

public enum ChatHaptic: Sendable { case light, soft }

/// A message Horizontal History asked the list to show. `token` changes on every ask, so the same message can be
/// jumped to twice.
public struct ChatJump: Equatable, Sendable {
    public var id: String
    public var token: Int
}

@MainActor
@Observable
public final class ChatEngine: LedgerSink {
    /// The long poll's wait while a turn is in flight somewhere, so its end shows at once.
    public static let busyWait = 2
    /// The long poll's wait otherwise.
    public static let idleWait = 25
    public static let busyLine = "Wait for Eve to finish, then tap your choice again."

    // ---- what the views read ------------------------------------------------------------------------------------

    public private(set) var messages: [ChatMessage] = []
    /// Sends waiting for the reply on the screen to end, oldest first.
    public private(set) var queued: [QueuedSend] = []
    /// A turn this app started is in flight: the composer shows Stop when it has nothing to send.
    public private(set) var running = false
    /// The first page of history has arrived.
    public private(set) var historyLoaded = false
    /// Why an owner message did not go, by message id.
    public private(set) var problems: [String: String] = [:]
    /// Horizontal History's cards, whether the row shows, and its one line when a star did not save.
    public private(set) var topics = ChatTopics()
    public var historyShown = false
    public private(set) var historyNote = ""
    /// The message the list should scroll to and glow.
    public private(set) var jump: ChatJump?
    /// Bumped when the list should scroll to its end whatever its place (a send, a queued send).
    public private(set) var sendTick = 0
    /// Bumped when a line arrived on its own (the long poll): the list follows only if it was at its end.
    public private(set) var arrivalTick = 0
    /// Bumped just before an older page goes in above, and again after: the list keeps its place across it.
    public private(set) var olderTick = 0

    /// eve is replying and no words are on the screen yet: this app's turn before its first words, or a turn the
    /// owner started elsewhere (Telegram, voice).
    public var showsTyping: Bool { waiting || (busyElsewhere && !running) }

    /// eve's reaction on an owner bubble, or "".
    public func reaction(for messageID: String) -> String {
        ridOf[messageID].flatMap { byRid[$0] } ?? ""
    }

    // ---- what the app sets --------------------------------------------------------------------------------------

    /// A light tap for a topic's star or jump; a soft one when a reply's first words, or a line from eve, arrive.
    @ObservationIgnored public var haptic: (ChatHaptic) -> Void = { _ in }

    // ---- private ------------------------------------------------------------------------------------------------

    @ObservationIgnored private weak var door: ChatDoor?
    @ObservationIgnored private let sleep: @MainActor (Double) async -> Void
    @ObservationIgnored private let now: () -> Double
    @ObservationIgnored private var ledger: Ledger!
    @ObservationIgnored private var queue = SendQueue()
    @ObservationIgnored private var inFlight = false
    @ObservationIgnored private var lastFailed = false
    @ObservationIgnored private var visible = true
    @ObservationIgnored private var cursor: String?
    @ObservationIgnored private var polling = false
    @ObservationIgnored private var epoch = 0
    @ObservationIgnored private var older = (loading: false, more: true)
    @ObservationIgnored private var jumps = 0
    private var waiting = false
    private var busyElsewhere = false
    private var ridOf: [String: String] = [:]
    private var byRid: [String: String] = [:]

    public init(door: ChatDoor, sleep: @escaping @MainActor (Double) async -> Void = ChatEngine.realSleep,
                now: @escaping () -> Double = { Date().timeIntervalSince1970 }) {
        self.door = door
        self.sleep = sleep
        self.now = now
        self.ledger = Ledger(sink: self, now: now)
    }

    public static func realSleep(_ seconds: Double) async {
        try? await Task.sleep(for: .seconds(seconds))
    }

    // ---- starting and showing -----------------------------------------------------------------------------------

    /// Read the history and keep the chat live. Call once the app is paired; calling it again starts over (a new
    /// pairing): what is on the screen goes and the history is read afresh.
    public func start() {
        epoch += 1
        messages = []
        queue = SendQueue()
        queued = []
        problems = [:]
        ridOf = [:]
        byRid = [:]
        topics = ChatTopics()
        historyShown = false
        historyNote = ""
        historyLoaded = false
        cursor = nil
        older = (false, true)
        inFlight = false
        running = false
        waiting = false
        busyElsewhere = false
        lastFailed = false
        ledger = Ledger(sink: self, now: now)
        let run = epoch
        Task { [weak self] in await self?.loadHistory(run) }
    }

    /// The chat shows, or it does not (the app is in the background, or another tab is up). Hidden, the long poll
    /// stops; shown again, it resumes, a send that waited goes, and the topics are read again.
    public func setVisible(_ shown: Bool) {
        guard shown != visible else { return }
        visible = shown
        guard shown else { return }
        kick()
        drain()
        refreshTopics()
    }

    private func loadHistory(_ run: Int) async {
        // Retried quietly with backoff: the list stays empty until eve answers, never an error box.
        var backoff = 1.0
        while run == epoch {
            if let door, case .success(let json) = await door.messages(after: nil, wait: 0, before: nil),
               let payload = MessagesPayload.parse(json) {
                guard run == epoch else { return }
                ledger.markSeen(payload.messages)
                ingestReactions(payload)
                cursor = payload.cursor
                // Anything this app already sent in this run stays, after the history it was read with.
                messages = ChatLines.map(payload.messages) + messages
                historyLoaded = true
                drain()
                kick()
                refreshTopics()
                return
            }
            await sleep(backoff)
            backoff = min(backoff * 2, 15)
        }
    }

    // ---- the long poll ------------------------------------------------------------------------------------------

    private func kick() {
        guard !polling, visible, historyLoaded else { return }
        let run = epoch
        Task { [weak self] in await self?.poll(run) }
    }

    private func poll(_ run: Int) async {
        guard !polling else { return }
        polling = true
        defer { polling = false }
        var backoff = 1.0
        while run == epoch, visible, let door, let after = cursor {
            // While busy, a short wait: the daemon wakes a long poll on a new line, and eve's last line is written
            // while the turn is still in flight, so a 25 s wait kept the "replying" dots up that long.
            let result = await door.messages(after: after, wait: busyElsewhere ? Self.busyWait : Self.idleWait,
                                             before: nil)
            guard run == epoch else { return }
            guard case .success(let json) = result, let payload = MessagesPayload.parse(json) else {
                await sleep(backoff)
                backoff = min(backoff * 2, 15)
                continue
            }
            if !payload.cursor.isEmpty { cursor = payload.cursor }
            ledger.ingest(payload.messages)
            ingestReactions(payload)
            busyElsewhere = payload.busy
            backoff = 1
        }
    }

    private func ingestReactions(_ payload: MessagesPayload) {
        for w in payload.messages where w.role == .owner {
            if let rid = w.rid { ridOf[w.id] = rid }
        }
        if let set = payload.reactions {
            if set != byRid { byRid = set }
        } else {
            // An older daemon: only the rows' own reactions.
            for w in payload.messages where w.role == .owner {
                if let rid = w.rid, !w.reaction.isEmpty { byRid[rid] = w.reaction }
            }
        }
    }

    // ---- sending ------------------------------------------------------------------------------------------------

    /// The composer's words, and what it uploaded with them. A send made while eve replies waits its turn.
    public func send(_ text: String, attachments: [ChatAttachment] = []) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty || !attachments.isEmpty else { return }
        enqueue(text, wire: nil, attachments: attachments)
    }

    /// A card's tap that continues the conversation: `human` shows as the owner's bubble, and goes to eve with what
    /// was clicked and any form values (`data`, JSON values). Returns a line to show when it did not go, else "".
    public func ask(_ human: String, data: [Any] = []) -> String {
        let human = human.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !human.isEmpty else { return "" }
        // One card action at a time, and never into a running reply: a tap is an answer to what is on the screen.
        if inFlight || !queue.items.isEmpty { return Self.busyLine }
        enqueue(human, wire: OwnerAction.text(human: human, extra: data), attachments: [])
        return ""
    }

    private func enqueue(_ text: String, wire: String?, attachments: [ChatAttachment]) {
        queue.push(text, wire: wire, attachments: attachments)
        queued = queue.items
        sendTick += 1
        drain()
    }

    /// Start the oldest queued send when nothing of this app's is running. It leaves the queue in the same step that
    /// puts it in the thread, so its "Queued" bubble and the real one never both show.
    private func drain() {
        let gate = StartGate(inFlight: inFlight, historyLoaded: historyLoaded,
                             hiddenAfterError: lastFailed && !visible)
        guard let next = queue.take(gate) else { return }
        queued = queue.items
        begin(next)
    }

    private func begin(_ next: QueuedSend) {
        guard let door else { return }
        inFlight = true
        running = true
        waiting = true
        lastFailed = false
        // The daemon keys this message's reaction by its client_id: the bubble wears it.
        let clientID = UUID().uuidString.lowercased()
        let owner = ChatMessage(id: "local:\(clientID)", role: .owner, text: next.text, attachments: next.attachments)
        messages.append(owner)
        ridOf[owner.id] = clientID
        sendTick += 1
        let sent = next.wire ?? next.text
        let claim = ledger.beginTurn(sent: sent, userID: owner.id)
        let replyID = "reply:\(clientID)"
        let run = epoch
        var felt = false
        door.turn(id: clientID, text: sent, clientID: clientID, attachments: next.attachments.map(\.id),
                  onChunk: { [weak self] chunk in
                      guard let self, run == self.epoch, !claim.ended else { return }
                      claim.bytes(chunk)
                      guard !claim.text.isEmpty else { return }       // a keep-alive is not a reply
                      self.waiting = false
                      self.showReply(replyID, claim.text, streaming: true)
                      if !felt {
                          felt = true
                          self.haptic(.soft)
                      }
                  },
                  onEnd: { [weak self] end in
                      guard let self, run == self.epoch, !claim.ended else { return }
                      claim.end()
                      if !claim.text.isEmpty { self.showReply(replyID, claim.text, streaming: false) }
                      if case .failed(let why) = end, claim.text.isEmpty { self.problems[owner.id] = why }
                      self.settle(end)
                  })
    }

    private func showReply(_ id: String, _ text: String, streaming: Bool) {
        if let i = messages.lastIndex(where: { $0.id == id }) {
            if messages[i].text != text { messages[i].text = text }
            if messages[i].streaming != streaming { messages[i].streaming = streaming }
        } else {
            messages.append(ChatMessage(id: id, role: .eve, text: text, streaming: streaming))
        }
    }

    /// The turn is over: lines the poll held during it go in (a picture sent mid-turn, before the reply), the dots
    /// stop waiting on a long poll, and the next queued send starts.
    private func settle(_ end: TurnEnd) {
        inFlight = false
        running = false
        waiting = false
        lastFailed = end != .done
        ledger.flushIfIdle()
        busyElsewhere = false
        drain()
        kick()
        refreshTopics()
    }

    // ---- LedgerSink ---------------------------------------------------------------------------------------------

    public var idle: Bool { !inFlight }

    public func append(_ new: [ChatMessage]) {
        messages.append(contentsOf: new)
        arrivalTick += 1
        if new.contains(where: { $0.role == .eve }) { haptic(.soft) }
        // The Mac has the line after all: whatever said it did not go is stale.
        if !problems.isEmpty, new.contains(where: { $0.role == .eve }) { problems = [:] }
    }

    public func insertBeforeReply(_ new: [ChatMessage]) -> Bool {
        guard let last = messages.last, last.role == .eve, messages.contains(where: { $0.role == .owner })
        else { return false }
        messages.insert(contentsOf: new, at: messages.count - 1)
        arrivalTick += 1
        return true
    }

    public func attachSources(_ sources: [ChatSource], userID: String?) {
        guard let i = ChatLines.replyIndex(in: messages, after: userID) else { return }
        messages[i].sources = ChatSource.merged(messages[i].sources + sources)
    }

    // ---- infinite scroll ----------------------------------------------------------------------------------------

    /// The list is near its top: fetch the page before the first loaded message and put it above. Nothing is ever
    /// pruned on the Mac, so this goes back to the first day; `more: false` ends it.
    public func loadOlder() {
        guard !older.loading, older.more, historyLoaded, let door,
              let first = messages.first(where: { ChatMessage.isTranscriptID($0.id) }) else { return }
        older.loading = true
        let run = epoch
        Task { [weak self] in
            let result = await door.messages(after: nil, wait: 0, before: first.id)
            guard let self, run == self.epoch else { return }
            self.older.loading = false                              // a failure is tried again on the next scroll up
            guard case .success(let json) = result, let payload = MessagesPayload.parse(json) else { return }
            self.older.more = payload.more == true
            self.ledger.markSeen(payload.messages)
            self.ingestReactions(payload)
            let page = ChatLines.map(payload.messages)
            let known = Set(self.messages.map(\.id))
            let fresh = page.filter { !known.contains($0.id) }
            guard !fresh.isEmpty else { return }
            self.olderTick += 1
            self.messages.insert(contentsOf: fresh, at: 0)
        }
    }

    /// Whether still older messages may exist.
    public var hasOlder: Bool { older.more }

    // ---- Horizontal History -------------------------------------------------------------------------------------

    /// The composer's history button: open or close the row of topic cards. Opening asks the Mac for fresh topics.
    public func toggleHistory() {
        historyShown.toggle()
        if historyShown { refreshTopics() }
    }

    private func refreshTopics() {
        guard let door, historyLoaded else { return }
        let run = epoch
        Task { [weak self] in
            // No cards is a quiet state; the next turn or visit asks again.
            guard case .success(let json) = await door.topics(), let self, run == self.epoch else { return }
            self.topics = ChatTopics.parse(json)
        }
    }

    /// A tapped card: jump to the topic's first loaded message, or, when none of it is loaded, pick it back up as
    /// the owner's next message and let eve's memory do the rest.
    public func open(_ topic: ChatTopic) {
        haptic(.light)
        historyShown = false
        if let start = topic.start(in: messages.map(\.id)) {
            jumps += 1
            jump = ChatJump(id: start, token: jumps)
        } else {
            send(topic.pickUp)
        }
    }

    /// Save or unsave a topic: shown at once, and put back with one line if the Mac refuses.
    public func star(_ topic: ChatTopic) {
        guard let door else { return }
        let on = !topic.bookmarked
        haptic(.light)
        topics = topics.withBookmark(topic.id, on: on)
        historyNote = ""
        let run = epoch
        Task { [weak self] in
            guard case .failure = await door.bookmark(topic: topic.id, on: on), let self, run == self.epoch
            else { return }
            self.topics = self.topics.withBookmark(topic.id, on: !on)
            self.historyNote = on ? "Couldn't save that topic. Try again." : "Couldn't remove that topic. Try again."
        }
    }
}
