// The Work tab's sessions (docs/work-sessions.md): Claude Code sessions on the Mac, in any terminal (Warp, Terminal,
// an IDE). The phone lists them, starts one with a task as its first prompt, reads a session's latest turns, sends a
// live one the owner's messages, resumes an ended one, and stops one eve started. Foundation only, so Tests/main.swift
// checks the decoding, the requests and the outbox on the Mac. The calls go through WorkClient's `call` (the link
// token, the rendezvous retry).
//
//   GET  /api/link/sessions               {"ok", "folder", "sessions": [S]}; 503 when Work sessions are off
//   POST /api/link/sessions/start         {"task", "client_id"} → {"ok": true, "session": S}; the same client_id again is
//                                         the same session, never a second terminal
//   GET  /api/link/sessions/<id>          {"ok", "session": S, "turns": [{"role", "text", "at"}], "cut", "ended"};
//                                         an ended session's S has state "ended" (either one marks it ended)
//   POST /api/link/sessions/<id>/reply    {"text"} → {"ok": true, "delivered": "now" | "queued" | "held" | "typed",
//                                         "reason"?}; 409 {"reason", "code"?} when it can't take it ("code": "ended"
//                                         once it has ended). An older eve answers {"ok": true} alone.
//   POST /api/link/sessions/<id>/stop     {"ok": true, "stopped": true}
//   POST /api/link/sessions/<id>/continue {"text", "client_id"} → {"ok": true, "delivered": "resumed", "session": S}:
//                                         an ended session opened again, the same session under the same id, the
//                                         text its next prompt (a live one gets the text as a reply would, and
//                                         answers as a reply does, with "session")
//
//   S = {"id", "folder", "title", "state": starting | working | waiting | idle | ended, "last_activity": epoch seconds,
//        "last_line", "from_work", "can_reply", "can_stop", "can_continue", "where"}
//   can_reply: live, and eve can reach it, whatever terminal it runs in. can_continue: ended, and it can be resumed.
//   where: the app it runs in ("Warp", "Terminal", "VS Code"), or "". An older eve leaves "where" out.
import Foundation

/// Where a session is. The daemon's rule (state.py agent_rows): waiting on the owner beats working beats idle.
enum WorkSessionState: String, Sendable, CaseIterable {
    case starting, working, waiting, idle, ended

    /// The row's chip.
    var label: String {
        switch self {
        case .starting: "Starting"
        case .working: "Working"
        case .waiting: "Waiting on you"
        case .idle: "Idle"
        case .ended: "Ended"
        }
    }

    /// The list's order: the ones that need the owner first, then the busy ones, then the quiet ones.
    var rank: Int {
        switch self {
        case .waiting: 0
        case .starting, .working: 1
        case .idle: 2
        case .ended: 3
        }
    }

    var live: Bool { self != .ended }
}

/// One Claude Code session as the daemon lists it.
struct WorkSession: Equatable, Sendable, Identifiable {
    static let maxTitle = 80
    static let maxLine = 200
    static let maxWhere = 24

    var id: String
    var folder: String
    var title: String
    var state: WorkSessionState
    var lastActivity: Date?
    var lastLine: String
    /// eve started it from Work (only those take a stop).
    var fromWork: Bool
    /// Live, and eve can reach it, whatever terminal it runs in: the sheet's message box.
    var canReply: Bool
    var canStop: Bool
    /// Ended, and eve can open the same session again (Resume).
    var canContinue: Bool
    /// The app it runs in on the Mac ("Warp", "Terminal", "VS Code"), or "" (an ended one, or an older eve).
    var runsIn: String

    init(id: String, folder: String = "", title: String = "", state: WorkSessionState = .idle,
         lastActivity: Date? = nil, lastLine: String = "", fromWork: Bool = false, canReply: Bool = false,
         canStop: Bool = false, canContinue: Bool = false, runsIn: String = "") {
        self.id = id; self.folder = folder; self.title = title; self.state = state; self.lastActivity = lastActivity
        self.lastLine = lastLine; self.fromWork = fromWork; self.canReply = canReply; self.canStop = canStop
        self.canContinue = canContinue; self.runsIn = runsIn
    }

    /// nil without an id. An unknown state reads as idle (a newer eve's word for something quiet); the texts are
    /// clipped to the contract's lengths whatever the daemon sent. A field an older eve leaves out reads as false or "".
    init?(json: [String: Any]) {
        guard let id = json["id"] as? String, !id.isEmpty else { return nil }
        let at = (json["last_activity"] as? NSNumber)?.doubleValue
        self.init(id: id,
                  folder: Self.clean(json["folder"] as? String, Self.maxTitle),
                  title: Self.clean(json["title"] as? String, Self.maxTitle),
                  state: WorkSessionState(rawValue: json["state"] as? String ?? "") ?? .idle,
                  lastActivity: at.flatMap { $0 > 0 ? Date(timeIntervalSince1970: $0) : nil },
                  lastLine: Self.clean(json["last_line"] as? String, Self.maxLine),
                  fromWork: json["from_work"] as? Bool ?? false,
                  canReply: json["can_reply"] as? Bool ?? false,
                  canStop: json["can_stop"] as? Bool ?? false,
                  canContinue: json["can_continue"] as? Bool ?? false,
                  runsIn: Self.clean(json["where"] as? String, Self.maxWhere))
    }

    /// The row's first line: the title, else what it last said, else its folder.
    var headline: String {
        if !title.isEmpty { return title }
        if !lastLine.isEmpty { return lastLine }
        return folder.isEmpty ? "Claude Code" : folder
    }

    /// The row's muted line: what it last said, when the headline is not already that.
    var subline: String { title.isEmpty ? "" : lastLine }

    /// Where it runs and in which folder, "Warp · eve": the row's and the sheet's small line.
    var place: String { [runsIn, folder].filter { !$0.isEmpty }.joined(separator: " · ") }

    /// The same for VoiceOver: "In Warp, folder eve".
    var placeSpoken: String {
        switch (runsIn.isEmpty, folder.isEmpty) {
        case (true, true): ""
        case (true, false): "In \(folder)"
        case (false, true): "In \(runsIn)"
        case (false, false): "In \(runsIn), folder \(folder)"
        }
    }

    /// One line, whitespace runs folded to a space, at most `max` characters.
    static func clean(_ s: String?, _ max: Int) -> String {
        let words = (s ?? "").split(whereSeparator: { $0.isWhitespace || $0.isNewline })
        return String(words.joined(separator: " ").prefix(max))
    }

    /// "just now", "4 min ago", "2 h ago", "3 d ago": short enough for a row.
    static func ago(_ date: Date?, now: Date) -> String {
        guard let date else { return "" }
        let secs = max(0, now.timeIntervalSince(date))
        if secs < 60 { return "just now" }
        if secs < 3600 { return "\(Int(secs / 60)) min ago" }
        if secs < 86_400 { return "\(Int(secs / 3600)) h ago" }
        return "\(Int(secs / 86_400)) d ago"
    }
}

struct WorkSessionList: Equatable, Sendable {
    /// The work folder's name, as the daemon calls it.
    var folder: String
    var sessions: [WorkSession]

    var waiting: Int { sessions.filter { $0.state == .waiting }.count }

    /// Waiting on the owner first, then working, then idle; newest activity first within each.
    static func ordered(_ sessions: [WorkSession]) -> [WorkSession] {
        sessions.enumerated().sorted { a, b in
            if a.element.state.rank != b.element.state.rank { return a.element.state.rank < b.element.state.rank }
            let ta = a.element.lastActivity?.timeIntervalSince1970 ?? 0
            let tb = b.element.lastActivity?.timeIntervalSince1970 ?? 0
            return ta != tb ? ta > tb : a.offset < b.offset
        }.map(\.element)
    }

    static func decode(_ data: Data) -> WorkSessionList? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let raw = obj["sessions"] as? [[String: Any]] else { return nil }
        var seen = Set<String>()
        let sessions = raw.compactMap(WorkSession.init(json:)).filter { seen.insert($0.id).inserted }
        return WorkSessionList(folder: WorkSession.clean(obj["folder"] as? String, WorkSession.maxTitle),
                               sessions: ordered(sessions))
    }
}

/// One turn of a session: the owner's words or Claude's.
struct WorkSessionTurn: Equatable, Sendable, Identifiable {
    enum Role: String, Sendable { case owner, claude }
    static let maxText = 2000

    var id: Int                 // its place in the detail, oldest first
    var role: Role
    var text: String
    var at: Date?
}

struct WorkSessionDetail: Equatable, Sendable {
    var session: WorkSession
    var turns: [WorkSessionTurn]
    /// Older turns were left out.
    var cut: Bool

    static func decode(_ data: Data) -> WorkSessionDetail? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let raw = obj["session"] as? [String: Any], var session = WorkSession(json: raw) else { return nil }
        if obj["ended"] as? Bool == true { session.state = .ended }
        var turns: [WorkSessionTurn] = []
        for t in obj["turns"] as? [[String: Any]] ?? [] {
            guard let role = WorkSessionTurn.Role(rawValue: t["role"] as? String ?? ""),
                  let text = t["text"] as? String,
                  !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { continue }
            let at = (t["at"] as? NSNumber)?.doubleValue
            turns.append(WorkSessionTurn(id: turns.count, role: role, text: String(text.prefix(WorkSessionTurn.maxText)),
                                         at: at.flatMap { $0 > 0 ? Date(timeIntervalSince1970: $0) : nil }))
        }
        return WorkSessionDetail(session: session, turns: turns, cut: obj["cut"] as? Bool ?? false)
    }
}

/// What a start came to: the new session, or why there is none.
struct WorkSessionStart: Equatable, Sendable {
    var session: WorkSession?
    var reason: String?

    static func decode(_ data: Data) -> WorkSessionStart {
        let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        let ok = obj["ok"] as? Bool ?? false
        let session = ok ? (obj["session"] as? [String: Any]).flatMap(WorkSession.init(json:)) : nil
        let reason = (obj["reason"] as? String) ?? (obj["error"] as? String)
        return WorkSessionStart(session: session,
                                reason: session == nil ? (reason ?? "eve didn't start a session.") : nil)
    }
}

/// How a message reached a live session, as the reply answers it.
enum WorkDelivery: String, Sendable, CaseIterable {
    /// In the session now: an idle one wakes on it.
    case now
    /// The session is busy: it takes the message at its next step.
    case queued
    /// The session holds it until the owner approves it on the Mac.
    case held
    /// Typed into its Terminal tab (a Claude Code too old to take messages, in a tab eve opened).
    case typed

    /// The small line under the owner's bubble.
    var caption: String {
        switch self {
        case .now, .typed: "Sent"
        case .queued: "It's working: it'll take this at its next step"
        case .held: "Waiting for approval on the Mac"
        }
    }
}

/// What a reply or a resume came to.
struct WorkSendResult: Equatable, Sendable {
    var ok: Bool
    /// How it landed; nil from an older eve (it typed the reply) or for a resume.
    var delivered: WorkDelivery?
    var reason: String?
    /// A resume's answer: the session as opened again.
    var session: WorkSession?

    /// The bubble's line once it has gone.
    var caption: String { delivered?.caption ?? "Sent" }

    static func decode(_ data: Data) -> WorkSendResult {
        let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        let ok = obj["ok"] as? Bool ?? false
        let reason = ((obj["reason"] as? String) ?? (obj["error"] as? String)).flatMap { $0.isEmpty ? nil : $0 }
        return WorkSendResult(ok: ok, delivered: WorkDelivery(rawValue: obj["delivered"] as? String ?? ""),
                              reason: reason,
                              session: ok ? (obj["session"] as? [String: Any]).flatMap(WorkSession.init(json:)) : nil)
    }
}

/// What the sheet's bottom is for a session: a message box, Resume, or a line saying why neither.
enum WorkComposer: Equatable, Sendable {
    case message
    case resume
    case note(String)

    /// A live session that eve can reach takes messages, and so does one still starting (they wait on the phone until
    /// it is up) or one a resume is bringing back. An ended one can be resumed when the daemon says so. Never Resume for
    /// a live one.
    static func of(_ s: WorkSession, resuming: Bool) -> WorkComposer {
        if resuming { return .message }
        if !s.state.live { return s.canContinue ? .resume : .note("This session has ended.") }
        if s.canReply || s.state == .starting { return .message }
        return .note(s.runsIn.isEmpty ? "eve can't reach this session from the phone."
                                      : "eve can't reach this session in \(s.runsIn) from the phone.")
    }
}

/// The owner's messages to one session, from the tap until the session's transcript shows them. Each shows at once
/// as the owner's bubble; they go to the Mac one at a time, in order, and the box never waits for one. A failed one
/// pauses the ones behind it until the owner tries again, so the session never gets them out of order.
struct WorkOutbox: Equatable, Sendable {
    enum Kind: Equatable, Sendable {
        case message
        /// The words an ended session is opened again with. `clientID`: a retry is the same resume.
        case resume(clientID: String)
    }

    enum Phase: Equatable, Sendable {
        /// On the phone, behind an earlier one.
        case waiting
        case sending
        /// Gone; what eve said, in words ("Sent", "Waiting for approval on the Mac").
        case sent(String)
        /// Why not. The queue stops here until the owner tries again.
        case failed(String)

        var unsent: Bool {
            switch self {
            case .waiting, .failed: true
            case .sending, .sent: false
            }
        }
    }

    struct Message: Equatable, Sendable, Identifiable {
        var id: Int
        var text: String
        var kind: Kind
        var phase: Phase
        var typedAt: Date
        /// When it went (a retry can be long after the tap): `keepSent` counts from here.
        var sentAt: Date?
        /// The owner's turns the sheet showed when this was typed: none of them is this message coming back.
        var before: Set<String>

        /// The session holds it until the owner approves it on the Mac.
        var held: Bool { phase == .sent(WorkDelivery.held.caption) }
    }

    /// The phone's and the Mac's clocks can differ a little: a turn more than this older than the tap is not its echo.
    static let skew: TimeInterval = 120
    /// A sent message the transcript never shows (held and never approved, or an eve that leaves relayed words out of
    /// a session's turns) goes this long after it went.
    static let keepSent: TimeInterval = 30 * 60
    /// How the daemon masks a secret in a turn (work_sessions.MASK).
    static let mask = "[hidden]"

    private(set) var messages: [Message] = []
    private var lastID = 0
    /// Echoes already taken, by turn key and how many: a turn that dropped one bubble never drops a second (the owner
    /// can send "yes" twice). Forgotten once the outbox is empty: every turn then on screen is in the next one's
    /// `before`.
    private var taken: [String: Int] = [:]

    var isEmpty: Bool { messages.isEmpty }

    /// A message failed and waits on the owner: the list's row says so, since the sheet may be closed.
    var hasFailed: Bool { messages.contains { if case .failed = $0.phase { true } else { false } } }

    /// The next to send: the oldest not yet gone, when it waits. nil while one is on its way, and while the oldest
    /// unsent one has failed (the owner tries it again, or deletes it).
    var next: Message? {
        guard let m = messages.first(where: { if case .sent = $0.phase { false } else { true } }) else { return nil }
        return m.phase == .waiting ? m : nil
    }

    /// A resume that has not answered yet: the session is on its way back, so the box takes messages for it.
    var resuming: Bool {
        messages.contains { m in
            guard case .resume = m.kind else { return false }
            return m.phase == .waiting || m.phase == .sending
        }
    }

    /// The owner's words as a message, trimmed and at most WorkSessionRequests.maxReply; nil for only whitespace.
    @discardableResult
    mutating func add(_ text: String, shown: [WorkSessionTurn], at now: Date) -> Message? {
        let words = WorkSessionRequests.trimmed(text, WorkSessionRequests.maxReply)
        guard !words.isEmpty else { return nil }
        return append(words, .message, shown: shown, at: now)
    }

    /// The words an ended session opens again with. Messages that never went go with them, oldest first and the new
    /// words last, since the resumed session's next prompt is the only way in; past WorkSessionRequests.maxTask the
    /// oldest words are cut, never the new ones.
    @discardableResult
    mutating func addResume(_ text: String, clientID: String, shown: [WorkSessionTurn], at now: Date) -> Message? {
        let words = WorkSessionRequests.trimmed(text, WorkSessionRequests.maxTask)
        guard !words.isEmpty else { return nil }
        let earlier = messages.filter(\.phase.unsent).map(\.text)
        messages.removeAll { $0.phase.unsent }
        let all = (earlier + [words]).joined(separator: "\n\n")
        return append(String(all.suffix(WorkSessionRequests.maxTask)), .resume(clientID: clientID), shown: shown, at: now)
    }

    private mutating func append(_ text: String, _ kind: Kind, shown: [WorkSessionTurn], at now: Date) -> Message {
        lastID += 1
        let before = Set(shown.filter { $0.role == .owner }.map(Self.key))
        let m = Message(id: lastID, text: text, kind: kind, phase: .waiting, typedAt: now, before: before)
        messages.append(m)
        return m
    }

    mutating func begin(_ id: Int) { set(id, .sending) }

    /// What the send came to. A message already gone (its echo came first, or the owner deleted it) is left gone.
    mutating func settle(_ id: Int, _ phase: Phase, at now: Date = .now) {
        set(id, phase)
        if case .sent = phase, let i = messages.firstIndex(where: { $0.id == id }) { messages[i].sentAt = now }
    }

    private mutating func set(_ id: Int, _ phase: Phase) {
        if let i = messages.firstIndex(where: { $0.id == id }) { messages[i].phase = phase }
    }

    /// The failed ones wait to go again, in their places, and so the queue behind them moves.
    mutating func retry() {
        for i in messages.indices where messages[i].phase.unsent { messages[i].phase = .waiting }
    }

    /// The owner deleted one that has not gone.
    mutating func remove(_ id: Int) {
        messages.removeAll { $0.id == id && $0.phase.unsent }
    }

    /// The words of every message that has not gone, oldest first, taken out (an ended session: back into the box,
    /// for a resume).
    mutating func takeUnsent() -> String {
        let words = messages.filter(\.phase.unsent).map(\.text)
        messages.removeAll { $0.phase.unsent }
        return words.joined(separator: "\n\n")
    }

    /// Drops each message the transcript now shows as the owner's turn, and sent ones older than `keepSent`. A turn
    /// is a message's echo when it was not on screen at the tap, is not much older than the tap, reads the same (see
    /// `echoes`), and is not already another message's echo. One still waiting is never an echo's: it has not gone.
    /// A message that failed and whose echo comes did land after all, so it goes too. `cut`: the read left older turns
    /// out; a sent message older than every turn shown then goes, since its echo is among those left out (the sheet
    /// was closed while the session went on), unless the session holds it for approval.
    mutating func confirm(_ turns: [WorkSessionTurn], cut: Bool = false, now: Date) {
        var count: [String: Int] = [:]
        var free: [(turn: WorkSessionTurn, key: String)] = []
        for t in turns where t.role == .owner {
            let k = Self.key(t)
            count[k, default: 0] += 1
            if count[k, default: 0] > taken[k, default: 0] { free.append((t, k)) }
        }
        let oldest = cut ? turns.compactMap(\.at).min() : nil
        var used = Set<Int>()
        var kept: [Message] = []
        for m in messages {
            if m.phase == .waiting { kept.append(m); continue }
            let went = m.sentAt ?? m.typedAt
            if case .sent = m.phase, now.timeIntervalSince(went) > Self.keepSent { continue }
            let echo = free.first { f in
                !used.contains(f.turn.id) && !m.before.contains(f.key)
                    && (f.turn.at.map { $0.timeIntervalSince(m.typedAt) >= -Self.skew } ?? true)
                    && Self.echoes(f.turn.text, m.text)
            }
            if let echo {
                used.insert(echo.turn.id)
                taken[echo.key, default: 0] += 1
            } else if case .sent = m.phase, !m.held, let oldest, oldest.timeIntervalSince(went) > Self.skew {
                continue
            } else {
                kept.append(m)
            }
        }
        if kept != messages { messages = kept }
        if messages.isEmpty, !taken.isEmpty { taken = [:] }
    }

    /// The small line under each bubble, by message id. A run of bubbles that would say the same says it once, under
    /// the last. `failedHint` follows a failure ("Tap to try again.").
    func captions(failedHint: String = "") -> [Int: String] {
        var out: [Int: String] = [:]
        for (i, m) in messages.enumerated() {
            let line = Self.caption(m.phase)
            if i + 1 < messages.count, Self.caption(messages[i + 1].phase) == line { continue }
            if case .failed = m.phase, !failedHint.isEmpty { out[m.id] = line + " " + failedHint } else { out[m.id] = line }
        }
        return out
    }

    static func caption(_ phase: Phase) -> String {
        switch phase {
        case .waiting: "Waiting to send"
        case .sending: "Sending…"
        case .sent(let line): line
        case .failed(let why): "Not sent: \(why)"
        }
    }

    /// Whether a turn from the transcript is this message coming back. The Mac may show it a little changed: on one
    /// line, after eve's own first line (the one that says whose words these are, which ends with a colon), with
    /// secrets masked as "[hidden]", cut at 2000 characters with "…" (the daemon counts characters as Python does,
    /// in Unicode scalars), or with a closing envelope tag in the words escaped ("<\/", `unescaped`).
    static func echoes(_ turn: String, _ sent: String) -> Bool {
        let want = normal(unescaped(sent))
        guard !want.isEmpty else { return false }
        let length = turn.unicodeScalars.count
        let open = length >= WorkSessionTurn.maxText
            || (turn.hasSuffix("…") && length >= WorkSessionTurn.maxText - 40)
        let whole = normal(unescaped(turn))
        if matches(whole, want, open: open) { return true }
        guard let colon = whole.range(of: ": ") else { return false }
        return matches(String(whole[colon.upperBound...]), want, open: open)
    }

    /// `pattern` against `text`, each "[hidden]" in it standing for any run of characters; `open`: the pattern was
    /// cut, so the text may go on past it.
    static func matches(_ pattern: String, _ text: String, open: Bool) -> Bool {
        var p = pattern
        if open, p.hasSuffix("…") { p = String(p.dropLast()).trimmingCharacters(in: .whitespaces) }
        if p.isEmpty { return false }
        let parts = p.components(separatedBy: mask)
        if parts.count == 1 { return open ? text.hasPrefix(p) : text == p }
        guard text.hasPrefix(parts[0]) else { return false }
        var rest = text.dropFirst(parts[0].count)
        for (i, part) in parts.enumerated().dropFirst() {
            if i == parts.count - 1, !open { return rest.hasSuffix(part) }
            if part.isEmpty { continue }
            guard let r = rest.range(of: part) else { return false }
            rest = rest[r.upperBound...]
        }
        return true
    }

    /// The characters Claude Code reads as the "<" of a closing tag (claude_peer._OPENERS).
    static let openers: Set<Character> = Set("<＜﹤〈⟨〈‹˂ᐸ❬❮❰⧼≮≺⋖")

    /// The words without the backslash Claude Code puts after the "<" of anything that reads as a closing
    /// </cross-session-message> (claude_peer.escape_body), so the owner's words and the session's copy compare
    /// alike. Done to both, so words that already had "<\" still match themselves.
    static func unescaped(_ s: String) -> String {
        var out = ""
        var last: Character?
        for c in s {
            if c == "\\", let last, openers.contains(last) { continue }
            out.append(c)
            last = c
        }
        return out
    }

    /// Whitespace runs as one space, no space at either end.
    static func normal(_ s: String) -> String {
        s.split(whereSeparator: { $0.isWhitespace || $0.isNewline }).joined(separator: " ")
    }

    /// A turn as the outbox remembers it: its second and its words.
    static func key(_ t: WorkSessionTurn) -> String {
        "\(Int(t.at?.timeIntervalSince1970 ?? 0))|\(normal(t.text))"
    }
}

enum WorkSessionRequests {
    static let maxTask = 4000
    static let maxReply = 4000
    /// Opening a terminal and handing Claude its task takes a few seconds on the Mac.
    static let startTimeout: TimeInterval = 30

    static func trimmed(_ s: String, _ max: Int) -> String {
        String(s.trimmingCharacters(in: .whitespacesAndNewlines).prefix(max))
    }

    static func list(_ ep: ChatEndpoint) -> URLRequest { WorkRequests.request(ep, ["api", "link", "sessions"], post: false) }

    /// `clientID`: one per sheet and wording, so a retry after a dropped connection is the same session.
    static func start(_ ep: ChatEndpoint, task: String, clientID: String) -> URLRequest {
        var r = WorkRequests.request(ep, ["api", "link", "sessions", "start"], post: true,
                                     body: ["task": trimmed(task, maxTask), "client_id": String(clientID.prefix(64))])
        r.timeoutInterval = startTimeout
        return r
    }

    static func detail(_ ep: ChatEndpoint, id: String) -> URLRequest {
        WorkRequests.request(ep, ["api", "link", "sessions", id], post: false)
    }

    static func reply(_ ep: ChatEndpoint, id: String, text: String) -> URLRequest {
        WorkRequests.request(ep, ["api", "link", "sessions", id, "reply"], post: true,
                             body: ["text": trimmed(text, maxReply)])
    }

    static func stop(_ ep: ChatEndpoint, id: String) -> URLRequest {
        WorkRequests.request(ep, ["api", "link", "sessions", id, "stop"], post: true)
    }

    /// Resume: opens a terminal on the Mac like a start, so it gets a start's timeout. `clientID` as for a start.
    static func resume(_ ep: ChatEndpoint, id: String, text: String, clientID: String) -> URLRequest {
        var r = WorkRequests.request(ep, ["api", "link", "sessions", id, "continue"], post: true,
                                     body: ["text": trimmed(text, maxTask), "client_id": String(clientID.prefix(64))])
        r.timeoutInterval = startTimeout
        return r
    }
}

extension WorkClient {
    func sessions() async -> Result<WorkSessionList, ChatFailure> {
        await call(.sessions, WorkSessionRequests.list) { data, _ in WorkSessionList.decode(data) }
    }

    func startSession(task: String, clientID: String) async -> Result<WorkSessionStart, ChatFailure> {
        await call(.sessions, { WorkSessionRequests.start($0, task: task, clientID: clientID) }) { data, _ in
            WorkSessionStart.decode(data)
        }
    }

    func session(_ id: String) async -> Result<WorkSessionDetail, ChatFailure> {
        await call(.sessions, { WorkSessionRequests.detail($0, id: id) }) { data, _ in WorkSessionDetail.decode(data) }
    }

    func reply(_ text: String, to id: String) async -> Result<WorkSendResult, ChatFailure> {
        await call(.sessions, { WorkSessionRequests.reply($0, id: id, text: text) }) { data, _ in
            WorkSendResult.decode(data)
        }
    }

    func resumeSession(_ id: String, text: String, clientID: String) async -> Result<WorkSendResult, ChatFailure> {
        await call(.sessions, { WorkSessionRequests.resume($0, id: id, text: text, clientID: clientID) }) { data, _ in
            WorkSendResult.decode(data)
        }
    }

    func stopSession(_ id: String) async -> Result<WorkActionResult, ChatFailure> {
        await call(.sessions, { WorkSessionRequests.stop($0, id: id) }) { data, _ in WorkActionResult.decode(data) }
    }
}
