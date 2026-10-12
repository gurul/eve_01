// The Work tab's sessions (docs/work-sessions.md): Claude Code sessions on the Mac, each a terminal running `claude` in
// the owner's work folder. The phone lists them, starts one with a task as its first prompt, reads a session's latest
// turns, replies to it and stops it. Foundation only, so Tests/main.swift checks the decoding and the requests on the
// Mac. The calls go through WorkClient's `call` (the link token, the rendezvous retry).
//
//   GET  /api/link/sessions               {"ok", "folder", "sessions": [S]}; 503 when Work sessions are off
//   POST /api/link/sessions/start         {"task", "client_id"} → {"ok": true, "session": S}; the same client_id again is
//                                         the same session, never a second terminal
//   GET  /api/link/sessions/<id>          {"ok", "session": S, "turns": [{"role", "text", "at"}], "cut", "ended"};
//                                         an ended session's S has state "ended" (either one marks it ended)
//   POST /api/link/sessions/<id>/reply    {"text"} → {"ok": true}; 409 {"reason"} when it has ended (with
//                                         "code": "ended") or can't be reached
//   POST /api/link/sessions/<id>/stop     {"ok": true, "stopped": true}
//
//   S = {"id", "folder", "title", "state": starting | working | waiting | idle | ended, "last_activity": epoch seconds,
//        "last_line", "from_work", "can_reply", "can_stop"}
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

    var id: String
    var folder: String
    var title: String
    var state: WorkSessionState
    var lastActivity: Date?
    var lastLine: String
    /// eve started it from Work (only those take a reply or a stop).
    var fromWork: Bool
    var canReply: Bool
    var canStop: Bool

    init(id: String, folder: String = "", title: String = "", state: WorkSessionState = .idle,
         lastActivity: Date? = nil, lastLine: String = "", fromWork: Bool = false, canReply: Bool = false,
         canStop: Bool = false) {
        self.id = id; self.folder = folder; self.title = title; self.state = state; self.lastActivity = lastActivity
        self.lastLine = lastLine; self.fromWork = fromWork; self.canReply = canReply; self.canStop = canStop
    }

    /// nil without an id. An unknown state reads as idle (a newer eve's word for something quiet); the texts are
    /// clipped to the contract's lengths whatever the daemon sent.
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
                  canStop: json["can_stop"] as? Bool ?? false)
    }

    /// The row's first line: the title, else what it last said, else its folder.
    var headline: String {
        if !title.isEmpty { return title }
        if !lastLine.isEmpty { return lastLine }
        return folder.isEmpty ? "Claude Code" : folder
    }

    /// The row's muted line: what it last said, when the headline is not already that.
    var subline: String { title.isEmpty ? "" : lastLine }

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

    func reply(_ text: String, to id: String) async -> Result<WorkActionResult, ChatFailure> {
        await call(.sessions, { WorkSessionRequests.reply($0, id: id, text: text) }) { data, _ in
            WorkActionResult.decode(data)
        }
    }

    func stopSession(_ id: String) async -> Result<WorkActionResult, ChatFailure> {
        await call(.sessions, { WorkSessionRequests.stop($0, id: id) }) { data, _ in WorkActionResult.decode(data) }
    }
}
