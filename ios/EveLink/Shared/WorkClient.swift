// The Work view's network and data (docs/app-chat.md, "Work"): the chief's jobs and the owner's built apps (no longer
// listed in Work; eve's own controls are, ControlsClient.swift), read from
// the daemon through the tunnel with the link token, as the chat does (ChatTransport.swift). Foundation only, so
// Tests/main.swift checks the decoding and the requests on the Mac.
//
//   GET  /api/link/jobs                  {"jobs": [card + needs_you + actions], "needs_you": n}; 503 when the chief is off
//   POST /api/link/jobs                  New job: {"goal", "detail"?, "client_id"} heard as a chat turn; the card it
//                                        filed {"ok": true, "job": card, "said"}, or {"ok": false, "reason", "said"}
//   POST /api/link/jobs/<id>/<action>    go | done | drop | raise | happened | didnt: the chief's own methods;
//                                        a Go sends {"rev": n}, the card's revision the sheet showed; their dict back
//   GET  /api/link/apps                  {"apps": [{"slug", "name", "updated"}]}; 503 when the app builder is off
//   POST /api/link/apps/<slug>/open      {"url": "<tunnel origin>/apps/<slug>/?t=<app token>"}; 404 for no such app
import Foundation

/// What the owner can do to a job from the phone. The daemon lists only the ones that apply now: an act that may
/// already have happened gets "It happened" and "It didn't", never a Go.
enum WorkAction: String, CaseIterable, Sendable, Hashable {
    case go, done, drop, raise, happened, didnt, hide

    var title: String {
        switch self {
        case .go: "Go"
        case .done: "Done"
        case .drop: "Drop"
        case .raise: "Raise budget"
        case .happened: "It happened"
        case .didnt: "It didn't"
        case .hide: "Delete"
        }
    }
}

/// One step of a job (a chief card's phase).
struct WorkStep: Equatable, Sendable, Identifiable {
    var n: Int
    var kind: String            // research | assess | act | watch
    var goal: String
    var status: String          // queued | running | ok | failed | refused | skipped
    var oneWay: Bool            // needs the owner's Go before it runs
    var id: Int { n }

    var finished: Bool { ["ok", "skipped", "failed", "refused"].contains(status) }

    /// The step's state in a word or two, for the detail sheet and VoiceOver.
    var statusWord: String {
        switch status {
        case "queued": oneWay ? "Needs your Go" : "Next"
        case "running": "Running"
        case "ok": "Done"
        case "skipped": "Skipped"
        case "failed": "Failed"
        case "refused": "Refused"
        default: status.capitalized
        }
    }
}

/// One job: a chief card as `chief._as_data` gives it, plus what it needs from the owner.
struct WorkJob: Equatable, Sendable, Identifiable {
    var id: String
    var title: String
    var status: String          // proposed | active | waiting | unverified | done | dropped
    var waitingFor: String?     // question | go | budget | mac_slot
    var endState: String
    var deadline: String?
    var steps: [WorkStep]
    var spentUSD: Double?
    var budgetUSD: Double?
    var needsYou: Bool
    var actions: [WorkAction]
    /// The card's revision as listed: a Go sends it, and the chief refuses a card changed since.
    var rev: Int?
    /// The daemon's own status line and steps-done count (link_chat.job_line), when it sends them: it knows things
    /// the card alone does not, such as an act that may have happened.
    var line: String?
    var doneSteps: Int?

    var open: Bool { status != "done" && status != "dropped" }
    var stepsDone: Int { min(doneSteps ?? steps.filter(\.finished).count, steps.count) }
    /// 0...1 for the thin progress line; a job with no steps is all or nothing.
    var progress: Double {
        if steps.isEmpty { return status == "done" ? 1 : 0 }
        return Double(stepsDone) / Double(steps.count)
    }

    /// The one muted line under the title: the daemon's, else made here.
    var statusLine: String {
        if let line, !line.isEmpty { return line }
        switch status {
        case "done": return "Done"
        case "dropped": return "Dropped"
        case "proposed": return "Proposed"
        case "unverified": return "Finished, not checked"
        case "waiting":
            switch waitingFor {
            case "go": return "Waiting on your Go"
            case "question": return "Waiting on your answer"
            case "budget": return "Waiting on a budget raise"
            case "mac_slot": return "Waiting for the Mac"
            default: return "Waiting"
            }
        default:
            switch steps.first(where: { $0.status == "running" })?.kind {
            case "research": return "Researching"
            case "assess": return "Checking"
            case "act": return "Doing it"
            case "watch": return "Watching"
            default: return "Working"
            }
        }
    }

    init(id: String, title: String, status: String, waitingFor: String? = nil, endState: String = "",
         deadline: String? = nil, steps: [WorkStep] = [], spentUSD: Double? = nil, budgetUSD: Double? = nil,
         needsYou: Bool = false, actions: [WorkAction] = [], rev: Int? = nil, line: String? = nil,
         doneSteps: Int? = nil) {
        self.id = id; self.title = title; self.status = status; self.waitingFor = waitingFor
        self.endState = endState; self.deadline = deadline; self.steps = steps; self.spentUSD = spentUSD
        self.budgetUSD = budgetUSD; self.needsYou = needsYou; self.actions = actions
        self.rev = rev
        self.line = line; self.doneSteps = doneSteps
    }

    /// nil for a card without an id or a title; unknown actions are left out.
    init?(json: [String: Any]) {
        guard let id = json["id"] as? String, !id.isEmpty, let title = json["title"] as? String else { return nil }
        let steps = (json["phases"] as? [[String: Any]] ?? []).compactMap { p -> WorkStep? in
            guard let n = (p["n"] as? NSNumber)?.intValue else { return nil }
            return WorkStep(n: n, kind: p["do"] as? String ?? "", goal: p["goal"] as? String ?? "",
                            status: p["status"] as? String ?? "queued", oneWay: (p["door"] as? String) == "one_way")
        }
        self.init(id: id, title: title, status: json["status"] as? String ?? "active",
                  waitingFor: json["waiting_for"] as? String, endState: json["end_state"] as? String ?? "",
                  deadline: json["deadline"] as? String, steps: steps.sorted { $0.n < $1.n },
                  spentUSD: ((json["spent"] as? [String: Any])?["usd"] as? NSNumber)?.doubleValue,
                  budgetUSD: ((json["budget"] as? [String: Any])?["usd"] as? NSNumber)?.doubleValue,
                  needsYou: json["needs_you"] as? Bool ?? false,
                  actions: (json["actions"] as? [String] ?? []).compactMap(WorkAction.init(rawValue:)),
                  rev: (json["rev"] as? NSNumber)?.intValue,
                  line: json["line"] as? String, doneSteps: (json["steps_done"] as? NSNumber)?.intValue)
    }
}

struct WorkJobs: Equatable, Sendable {
    var jobs: [WorkJob]
    var needsYou: Int

    var open: [WorkJob] { jobs.filter(\.open) }
    var finished: [WorkJob] { jobs.filter { !$0.open } }

    static func decode(_ data: Data) -> WorkJobs? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let raw = obj["jobs"] as? [[String: Any]] else { return nil }
        let jobs = raw.compactMap(WorkJob.init(json:))
        let count = (obj["needs_you"] as? NSNumber)?.intValue ?? jobs.filter(\.needsYou).count
        return WorkJobs(jobs: jobs, needsYou: max(count, 0))
    }
}

/// One app the owner had eve build.
struct WorkApp: Equatable, Sendable, Identifiable {
    var slug: String
    var name: String
    var updated: String         // the app store's local ISO time, "2026-10-08T11:50:02"
    var id: String { slug }

    var updatedDate: Date? { Self.parse(updated) }

    static func parse(_ s: String) -> Date? {
        if s.isEmpty { return nil }
        let zoned = ISO8601DateFormatter()
        if let d = zoned.date(from: s) { return d }
        let local = DateFormatter()
        local.locale = Locale(identifier: "en_US_POSIX")
        local.timeZone = .current
        for format in ["yyyy-MM-dd'T'HH:mm:ss", "yyyy-MM-dd'T'HH:mm:ss.SSSSSS", "yyyy-MM-dd'T'HH:mm"] {
            local.dateFormat = format
            if let d = local.date(from: s) { return d }
        }
        return nil
    }

    static func decodeList(_ data: Data) -> [WorkApp]? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let raw = obj["apps"] as? [[String: Any]] else { return nil }
        return raw.compactMap { a in
            guard let slug = a["slug"] as? String, !slug.isEmpty else { return nil }
            let name = (a["name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? slug
            return WorkApp(slug: slug, name: name, updated: a["updated"] as? String ?? "")
        }
    }

    /// The open answer's URL, only when it is on eve's own tunnel (`base`'s scheme and host): the app sheet never
    /// loads anywhere else.
    static func decodeOpen(_ data: Data, base: URL) -> URL? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let s = obj["url"] as? String, let url = URL(string: s) else { return nil }
        return sameOrigin(url, base) ? url : nil
    }

    static func sameOrigin(_ a: URL, _ b: URL) -> Bool {
        a.scheme?.lowercased() == b.scheme?.lowercased() && a.host?.lowercased() == b.host?.lowercased()
            && a.port == b.port && ["https", "http"].contains(a.scheme?.lowercased() ?? "")
    }
}

/// What the chief said to an action: {"ok": true, ...} or {"ok": false, "reason": "..."}.
struct WorkActionResult: Equatable, Sendable {
    var ok: Bool
    var reason: String?

    static func decode(_ data: Data) -> WorkActionResult {
        let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        let reason = (obj["reason"] as? String) ?? (obj["error"] as? String)
        return WorkActionResult(ok: obj["ok"] as? Bool ?? false, reason: reason)
    }
}

/// What a New job came to: the card eve filed, or why there is none and what eve said instead.
struct WorkStartResult: Equatable, Sendable {
    var job: WorkJob?
    var reason: String?
    var said: [String]

    /// The sheet's line when no card was filed: eve's own words when it said any (a question back), else why.
    var problem: String {
        let words = said.joined(separator: "\n\n")
        if !words.isEmpty { return words }
        return reason ?? "eve didn't start a job."
    }

    /// A card only with "ok": true and a card that decodes; anything else is a result without one.
    static func decode(_ data: Data) -> WorkStartResult {
        let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        let ok = obj["ok"] as? Bool ?? false
        let job = ok ? (obj["job"] as? [String: Any]).flatMap(WorkJob.init(json:)) : nil
        let said = (obj["said"] as? [Any] ?? []).compactMap { $0 as? String }.filter { !$0.isEmpty }
        let reason = (obj["reason"] as? String) ?? (obj["error"] as? String)
        return WorkStartResult(job: job, reason: reason ?? (ok && job == nil ? "eve's answer didn't make sense." : nil),
                               said: said)
    }
}

enum WorkRequests {
    /// The longest a New job waits: eve hears it as a chat turn and answers once the card is filed (90 s at most).
    static let startTimeout: TimeInterval = 120
    static let maxGoal = 1000
    static let maxDetail = 2000

    /// `clientID`: one per sheet, so a retry after a dropped connection waits for the same turn, never a second card.
    static func start(_ ep: ChatEndpoint, goal: String, detail: String, clientID: String) -> URLRequest {
        var body: [String: Any] = ["goal": String(goal.trimmingCharacters(in: .whitespacesAndNewlines).prefix(maxGoal)),
                                   "client_id": clientID]
        let more = detail.trimmingCharacters(in: .whitespacesAndNewlines)
        if !more.isEmpty { body["detail"] = String(more.prefix(maxDetail)) }
        var r = request(ep, ["api", "link", "jobs"], post: true, body: body)
        r.timeoutInterval = startTimeout
        return r
    }

    static func request(_ ep: ChatEndpoint, _ path: [String], post: Bool,
                                body: [String: Any] = [:]) -> URLRequest {
        var url = ep.base
        for part in path { url = url.appending(component: part) }   // a "/" in a job id or slug is escaped
        var r = URLRequest(url: url)
        r.httpMethod = post ? "POST" : "GET"
        r.setValue("Bearer \(ep.token)", forHTTPHeaderField: "Authorization")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        if post {
            r.setValue("application/json", forHTTPHeaderField: "Content-Type")
            r.httpBody = (try? JSONSerialization.data(withJSONObject: body)) ?? Data("{}".utf8)
        }
        r.timeoutInterval = 20
        r.cachePolicy = .reloadIgnoringLocalCacheData
        return r
    }

    static func jobs(_ ep: ChatEndpoint) -> URLRequest { request(ep, ["api", "link", "jobs"], post: false) }
    /// `rev`: the revision the sheet showed, sent with a Go only, so a card changed since is refused.
    static func act(_ ep: ChatEndpoint, job: String, _ action: WorkAction, rev: Int? = nil) -> URLRequest {
        let body: [String: Any] = action == .go && rev != nil ? ["rev": rev!] : [:]
        return request(ep, ["api", "link", "jobs", job, action.rawValue], post: true, body: body)
    }
    static func apps(_ ep: ChatEndpoint) -> URLRequest { request(ep, ["api", "link", "apps"], post: false) }
    static func openApp(_ ep: ChatEndpoint, slug: String) -> URLRequest {
        request(ep, ["api", "link", "apps", slug, "open"], post: true)
    }
}

/// The Work view's calls. A failure to reach eve asks the rendezvous for its new address and tries once more, as the
/// chat does.
@MainActor
final class WorkClient {
    private let session: URLSession
    private let endpoint: @MainActor () -> ChatEndpoint?
    private let refresh: @MainActor () async -> Bool

    init(endpoint: @escaping @MainActor () -> ChatEndpoint?, refresh: @escaping @MainActor () async -> Bool) {
        self.endpoint = endpoint
        self.refresh = refresh
        let config = URLSessionConfiguration.default
        config.waitsForConnectivity = false
        config.httpCookieStorage = nil
        config.urlCache = nil
        session = URLSession(configuration: config)
    }

    /// Where eve is now, for a request this client does not make itself (Pika's video stream).
    func currentEndpoint() -> ChatEndpoint? { endpoint() }

    func jobs() async -> Result<WorkJobs, ChatFailure> {
        await call(.jobs, WorkRequests.jobs) { data, _ in WorkJobs.decode(data) }
    }

    func act(_ action: WorkAction, job: String, rev: Int? = nil) async -> Result<WorkActionResult, ChatFailure> {
        await call(.jobs, { WorkRequests.act($0, job: job, action, rev: rev) }) { data, _ in WorkActionResult.decode(data) }
    }

    func start(goal: String, detail: String, clientID: String) async -> Result<WorkStartResult, ChatFailure> {
        await call(.jobs, { WorkRequests.start($0, goal: goal, detail: detail, clientID: clientID) }) { data, _ in
            WorkStartResult.decode(data)
        }
    }

    func apps() async -> Result<[WorkApp], ChatFailure> {
        await call(.apps, WorkRequests.apps) { data, _ in WorkApp.decodeList(data) }
    }

    func open(_ slug: String) async -> Result<URL, ChatFailure> {
        await call(.apps, { WorkRequests.openApp($0, slug: slug) }) { data, ep in WorkApp.decodeOpen(data, base: ep.base) }
    }

    /// One request, retried once at eve's new address when the old one is gone (ControlsClient.swift uses it too).
    func call<T: Sendable>(_ kind: WorkFailure.Kind, _ build: (ChatEndpoint) -> URLRequest,
                                   _ decode: @escaping @Sendable (Data, ChatEndpoint) -> T?) async -> Result<T, ChatFailure> {
        var retried = false
        while true {
            guard let ep = endpoint() else {
                return .failure(ChatFailure(message: "Eve Link isn't paired.", addressMayHaveMoved: false))
            }
            let result = await Self.fetch(build(ep), kind: kind, session: session)
            switch result {
            case .success(let data):
                guard let value = decode(data, ep) else {
                    return .failure(ChatFailure(message: "eve's answer didn't make sense.", addressMayHaveMoved: false))
                }
                return .success(value)
            case .failure(let f):
                if f.addressMayHaveMoved, !retried, !Task.isCancelled, await refresh() {
                    retried = true
                    continue
                }
                return .failure(f)
            }
        }
    }

    @concurrent
    private static func fetch(_ request: URLRequest, kind: WorkFailure.Kind,
                              session: URLSession) async -> Result<Data, ChatFailure> {
        do {
            let (data, response) = try await session.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            guard status == 200 else { return .failure(WorkFailure.from(status: status, body: data, for: kind)) }
            return .success(data)
        } catch {
            return .failure(ChatFailure.from(error))
        }
    }
}

enum WorkFailure {
    /// Which half of the Work view asked: a 503 means its own part of eve is off.
    enum Kind: Sendable { case jobs, apps, controls }

    /// The chat's words, with the Work view's own for an off chief or app builder and an app that is gone.
    static func from(status: Int, body: Data, for kind: Kind) -> ChatFailure {
        switch status {
        case 503:
            let off = switch kind {
            case .jobs: "eve's chief is off."
            case .apps: "eve's app builder is off."
            case .controls: "eve's controls are off."
            }
            return ChatFailure(message: off, addressMayHaveMoved: false)
        case 404 where kind == .controls:
            // a control this Mac does not have says so ("eve has no TV on this computer."); an older eve, none
            let obj = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
            let line = (obj?["line"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            return ChatFailure(message: line ?? "This eve has no controls yet: update it on the Mac.",
                               addressMayHaveMoved: false)
        case 404: return ChatFailure(message: "That isn't there any more.", addressMayHaveMoved: false)
        default: return ChatFailure.from(status: status, body: body)
        }
    }
}
