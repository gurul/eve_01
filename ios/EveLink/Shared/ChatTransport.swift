// The chat's network, all native (docs/app-chat.md): the page asks, this code calls the daemon through the tunnel with
// the link token, and the answers go back to the page as text. The token never reaches JavaScript.
//
//   POST /api/link/turn       one typed turn, with the speed dial's grade; the response is eve's reply as
//                             OpenAI-style SSE, forwarded byte for byte (batched per main run loop turn) to the
//                             page's onChunk.
//   GET  /api/link/messages   the history and new lines, long-polled; the JSON goes to the page as one string.
//   POST /api/link/upload     one photo or file the composer attached, raw bytes; its id goes with the turn.
//   GET  /api/link/file/<id>  one attachment (the owner's or eve's) full size, to open in Quick Look.
//
// A connection failure asks the rendezvous (Rendezvous.swift, through Relay.refreshAddress) for eve's new address
// and tries once more. Retrying a turn is safe: the daemon hears a client_id once.
import Foundation

/// eve's address and the link token, from the Pairing.
struct ChatEndpoint: Equatable, Sendable {
    var base: URL
    var token: String
}

/// The composer's speed dial, sent with each turn as "grade". auto is the daemon's ordinary turn; instant is the brain's
/// model at low effort with no think_hard escalation; think runs the turn on the app's think model.
enum ChatGrade: String, CaseIterable, Sendable {
    case auto, instant, think

    var title: String {
        switch self {
        case .auto: "Auto"
        case .instant: "Instant"
        case .think: "Think"
        }
    }

    var detail: String {
        switch self {
        case .auto: "eve decides how long to think"
        case .instant: "Answers right away"
        case .think: "Thinks longer for better answers"
        }
    }

    /// The remembered choice; anything unknown is auto.
    static func stored(_ raw: String?) -> ChatGrade { raw.flatMap(ChatGrade.init(rawValue:)) ?? .auto }
    static let defaultsKey = "chat.grade"
}

enum ChatRequests {
    static func turn(_ ep: ChatEndpoint, text: String, clientID: String, grade: ChatGrade = .auto,
                     attachments: [String] = []) -> URLRequest {
        var r = URLRequest(url: ep.base.appendingPathComponent("api/link/turn"))
        r.httpMethod = "POST"
        r.setValue("Bearer \(ep.token)", forHTTPHeaderField: "Authorization")
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        var body: [String: Any] = ["text": text, "client_id": clientID, "grade": grade.rawValue]
        if !attachments.isEmpty { body["attachments"] = attachments }
        r.httpBody = try? JSONSerialization.data(withJSONObject: body)
        r.timeoutInterval = 45            // between bytes: the daemon pings every 15 s
        r.cachePolicy = .reloadIgnoringLocalCacheData
        return r
    }

    /// The raw bytes; the file's name percent-encoded in X-Filename (shown by eve, never used as a path).
    static func upload(_ ep: ChatEndpoint, data: Data, name: String, mime: String) -> URLRequest {
        var r = URLRequest(url: ep.base.appendingPathComponent("api/link/upload"))
        r.httpMethod = "POST"
        r.setValue("Bearer \(ep.token)", forHTTPHeaderField: "Authorization")
        r.setValue(mime.isEmpty ? "application/octet-stream" : mime, forHTTPHeaderField: "Content-Type")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        let safe = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~ "))
        let encoded = String(name.prefix(200)).addingPercentEncoding(withAllowedCharacters: safe) ?? "file"
        r.setValue(encoded.replacingOccurrences(of: " ", with: "%20"), forHTTPHeaderField: "X-Filename")
        r.httpBody = data
        r.timeoutInterval = 120
        r.cachePolicy = .reloadIgnoringLocalCacheData
        return r
    }

    /// One kept upload's bytes, full size (GET /api/link/file/<id>). The id is checked before it becomes a path.
    static func file(_ ep: ChatEndpoint, id: String) -> URLRequest? {
        guard ChatAttachments.isID(id) else { return nil }
        var r = URLRequest(url: ep.base.appendingPathComponent("api/link/file").appendingPathComponent(id))
        r.setValue("Bearer \(ep.token)", forHTTPHeaderField: "Authorization")
        r.timeoutInterval = 60
        r.cachePolicy = .reloadIgnoringLocalCacheData
        return r
    }

    /// Horizontal History's cards (GET /api/link/topics).
    static func topics(_ ep: ChatEndpoint) -> URLRequest {
        var r = URLRequest(url: ep.base.appendingPathComponent("api/link/topics"))
        r.setValue("Bearer \(ep.token)", forHTTPHeaderField: "Authorization")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        return r
    }

    /// Save or unsave a topic (POST /api/link/topics/bookmark {"id", "on"}).
    static func bookmark(_ ep: ChatEndpoint, topic: String, on: Bool) -> URLRequest {
        var r = URLRequest(url: ep.base.appendingPathComponent("api/link/topics/bookmark"))
        r.httpMethod = "POST"
        r.setValue("Bearer \(ep.token)", forHTTPHeaderField: "Authorization")
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        r.httpBody = try? JSONSerialization.data(withJSONObject: ["id": topic, "on": on])
        return r
    }

    static func messages(_ ep: ChatEndpoint, after: String?, wait: Int, before: String? = nil) -> URLRequest {
        var c = URLComponents(url: ep.base.appendingPathComponent("api/link/messages"), resolvingAgainstBaseURL: false)!
        var items: [URLQueryItem] = []
        if let after { items.append(URLQueryItem(name: "after", value: after)) }
        if let before { items.append(URLQueryItem(name: "before", value: before)) }
        if wait > 0 { items.append(URLQueryItem(name: "wait", value: String(wait))) }
        c.queryItems = items.isEmpty ? nil : items
        var r = URLRequest(url: c.url!)
        r.setValue("Bearer \(ep.token)", forHTTPHeaderField: "Authorization")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        r.timeoutInterval = TimeInterval(wait) + 15
        r.cachePolicy = .reloadIgnoringLocalCacheData
        return r
    }
}

/// Why a call failed, in words the page can show, and whether a new address might help.
struct ChatFailure: Error, Equatable, Sendable {
    var message: String
    var addressMayHaveMoved: Bool

    /// A failure to reach the tunnel at all. Cloudflare answers 530 (or 502) for a quick tunnel that is gone.
    static func from(_ error: Error) -> ChatFailure {
        guard let u = error as? URLError else { return ChatFailure(message: error.localizedDescription, addressMayHaveMoved: false) }
        switch u.code {
        case .cannotFindHost, .cannotConnectToHost, .dnsLookupFailed, .networkConnectionLost, .timedOut,
             .secureConnectionFailed, .badServerResponse:
            return ChatFailure(message: "Can't reach eve.", addressMayHaveMoved: true)
        case .notConnectedToInternet:
            return ChatFailure(message: "The phone is offline.", addressMayHaveMoved: false)
        default:
            return ChatFailure(message: u.localizedDescription, addressMayHaveMoved: false)
        }
    }

    static func from(status: Int, body: Data) -> ChatFailure {
        let said = (try? JSONSerialization.jsonObject(with: body) as? [String: Any])?["error"] as? String
        switch status {
        case 401: return ChatFailure(message: "Eve Link's pairing is out of date. Pair again from Telegram (/stick).",
                                     addressMayHaveMoved: false)
        case 403: return ChatFailure(message: "eve refused the request.", addressMayHaveMoved: false)
        case 503: return ChatFailure(message: said ?? "eve's chat isn't running.", addressMayHaveMoved: false)
        case 502, 530: return ChatFailure(message: "Can't reach eve.", addressMayHaveMoved: true)
        default: return ChatFailure(message: said ?? "eve answered \(status).", addressMayHaveMoved: false)
        }
    }
}

/// UTF-8 split at any byte: the complete characters now, the cut-off end kept for the next bytes.
enum UTF8Carry {
    /// How many leading bytes of `b` are whole characters (an unfinished sequence at the end is left out).
    static func completeCount(_ b: [UInt8]) -> Int {
        var i = b.count - 1
        var back = 0
        while i >= 0 && back < 4 {
            let c = b[i]
            if c & 0xC0 != 0x80 {           // ASCII or a lead byte
                let need = c < 0x80 ? 1 : c >= 0xF0 ? 4 : c >= 0xE0 ? 3 : c >= 0xC0 ? 2 : 1
                return b.count - i >= need ? b.count : i
            }
            i -= 1
            back += 1
        }
        return b.count                      // not UTF-8 we can finish: decode it as it is (lossy)
    }

    static func take(_ carry: inout [UInt8], _ bytes: [UInt8]) -> String {
        let all = carry + bytes
        let n = completeCount(all)
        carry = Array(all[n...])
        return String(decoding: all[..<n], as: UTF8.self)
    }
}

/// Bytes from a reading task to the page, in order. The reader pushes from any thread; the first push after a flush
/// schedules one flush, so everything that arrived during one run loop turn goes to the page as one call.
final class ByteForwarder: @unchecked Sendable {
    typealias Schedule = @Sendable (@escaping @Sendable () -> Void) -> Void
    static let mainQueue: Schedule = { work in DispatchQueue.main.async(execute: work) }

    private let lock = NSLock()
    private var pending: [UInt8] = []
    private var scheduled = false
    private var carry: [UInt8] = []         // touched only by flushes, which `schedule` runs one at a time
    private(set) var pushed = 0
    private let schedule: Schedule
    private let deliver: @Sendable (String) -> Void

    /// `schedule` must run its work items one at a time, in order (a serial queue, the main queue).
    init(schedule: @escaping Schedule = ByteForwarder.mainQueue, deliver: @escaping @Sendable (String) -> Void) {
        self.schedule = schedule
        self.deliver = deliver
    }

    var bytesPushed: Int { lock.withLock { pushed } }

    func push(_ bytes: [UInt8]) {
        guard !bytes.isEmpty else { return }
        let first: Bool = lock.withLock {
            pending += bytes
            pushed += bytes.count
            defer { scheduled = true }
            return !scheduled
        }
        if first { schedule { [self] in self.flush(final: false) } }
    }

    /// After everything pushed so far has been delivered, run `then`. Nothing may be pushed after this.
    func end(then: @escaping @Sendable () -> Void) {
        schedule { [self] in
            self.flush(final: true)
            then()
        }
    }

    private func flush(final: Bool) {
        let bytes: [UInt8] = lock.withLock {
            defer { pending = []; scheduled = false }
            return pending
        }
        var text = UTF8Carry.take(&carry, bytes)
        if final, !carry.isEmpty {
            text += String(decoding: carry, as: UTF8.self)
            carry = []
        }
        if !text.isEmpty { deliver(text) }
    }

    /// Read a byte stream into the forwarder, a line (or 1 KB) at a time.
    @concurrent
    static func pump<S: AsyncSequence>(_ bytes: S, into f: ByteForwarder) async throws where S.Element == UInt8 {
        var buf: [UInt8] = []
        buf.reserveCapacity(1024)
        for try await b in bytes {
            buf.append(b)
            if b == 0x0A || buf.count >= 1024 {
                f.push(buf)
                buf.removeAll(keepingCapacity: true)
            }
        }
        f.push(buf)
    }
}

/// One attachment, fetched full size to open in Quick Look.
struct ChatDownload: Sendable {
    var data: Data
    var mime: String

    /// The daemon's 404: the file expired (uploads are kept a day) or never was.
    static let gone = ChatFailure(message: "That file is gone. eve keeps pictures and files for a day.",
                                  addressMayHaveMoved: false)
}

/// How a turn's stream ended, as the page hears it.
enum ChatEnding: Equatable, Sendable {
    case done                   // onDone
    case error(String)          // onError
    case silent                 // the page cancelled it itself: nothing more to say
}

@MainActor
final class ChatTransport {
    private let session: URLSession
    private let endpoint: @MainActor () -> ChatEndpoint?
    private let refresh: @MainActor () async -> Bool
    private var tasks: [String: Task<Void, Never>] = [:]
    private var cancelledAs: [String: ChatEnding] = [:]
    private(set) var turnsInFlight: Set<String> = [] {
        didSet { if turnsInFlight.isEmpty != oldValue.isEmpty { onBusy?(!turnsInFlight.isEmpty) } }
    }
    /// A turn started streaming (true) or the last one ended (false).
    var onBusy: (@MainActor (Bool) -> Void)?

    init(endpoint: @escaping @MainActor () -> ChatEndpoint?, refresh: @escaping @MainActor () async -> Bool) {
        self.endpoint = endpoint
        self.refresh = refresh
        let config = URLSessionConfiguration.default
        config.waitsForConnectivity = false
        config.httpCookieStorage = nil
        config.urlCache = nil
        config.timeoutIntervalForResource = 330     // a turn streams for 300 s at most
        session = URLSession(configuration: config)
    }

    var busy: Bool { !turnsInFlight.isEmpty }

    /// POST the turn and stream the reply's raw SSE text to `onChunk`, then end with `onEnd`.
    func turn(id: String, text: String, clientID: String, grade: ChatGrade = .auto, attachments: [String] = [],
              onChunk: @escaping @MainActor @Sendable (String) -> Void,
              onEnd: @escaping @MainActor (ChatEnding) -> Void) {
        cancel(id: id, as: .silent)
        turnsInFlight.insert(id)
        let session = session
        tasks[id] = Task { [weak self] in
            var ending = ChatEnding.done
            var retried = false
            while true {
                guard let ep = self?.endpoint() else { ending = .error("Eve Link isn't paired."); break }
                let forwarder = ByteForwarder(deliver: { s in MainActor.assumeIsolated { onChunk(s) } })
                let failure = await Self.stream(ChatRequests.turn(ep, text: text, clientID: clientID, grade: grade,
                                                                  attachments: attachments),
                                                session: session, into: forwarder)
                await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in forwarder.end { c.resume() } }
                guard let failure else { break }
                if Task.isCancelled { break }
                if failure.addressMayHaveMoved, !retried, forwarder.bytesPushed == 0, let self, await self.refresh() {
                    retried = true
                    diagChat("chat: turn retried at eve's new address")
                    continue
                }
                ending = .error(failure.message)
                break
            }
            guard let self else { return }
            if Task.isCancelled { ending = self.cancelledAs[id] ?? .silent }
            self.cancelledAs[id] = nil
            self.tasks[id] = nil
            self.turnsInFlight.remove(id)
            onEnd(ending)
        }
    }

    /// GET the messages and hand back the response body as text.
    func messages(id: String, after: String?, wait: Int, before: String? = nil,
                  onEnd: @escaping @MainActor (Result<String, ChatFailure>) -> Void) {
        cancel(id: id, as: .silent)
        let session = session
        tasks[id] = Task { [weak self] in
            var result: Result<String, ChatFailure> = .failure(ChatFailure(message: "Eve Link isn't paired.",
                                                                           addressMayHaveMoved: false))
            var retried = false
            while let ep = self?.endpoint() {
                result = await Self.fetch(ChatRequests.messages(ep, after: after, wait: wait, before: before),
                                          session: session)
                if case .failure(let f) = result, f.addressMayHaveMoved, !retried, !Task.isCancelled,
                   let self, await self.refresh() {
                    retried = true
                    continue
                }
                break
            }
            guard let self else { return }
            let cancelled = Task.isCancelled
            self.cancelledAs[id] = nil
            self.tasks[id] = nil
            if !cancelled { onEnd(result) }
        }
    }

    /// One small JSON request for the page (Horizontal History), with one retry at eve's new address like
    /// ``messages``; the body comes back as text for onLink.
    func json(id: String, _ build: @escaping (ChatEndpoint) -> URLRequest,
              onEnd: @escaping @MainActor (Result<String, ChatFailure>) -> Void) {
        cancel(id: id, as: .silent)
        let session = session
        tasks[id] = Task { [weak self] in
            var result: Result<String, ChatFailure> = .failure(ChatFailure(message: "Eve Link isn't paired.",
                                                                           addressMayHaveMoved: false))
            var retried = false
            while let ep = self?.endpoint() {
                result = await Self.fetch(build(ep), session: session)
                if case .failure(let f) = result, f.addressMayHaveMoved, !retried, !Task.isCancelled,
                   let self, await self.refresh() {
                    retried = true
                    continue
                }
                break
            }
            guard let self else { return }
            let cancelled = Task.isCancelled
            self.cancelledAs[id] = nil
            self.tasks[id] = nil
            if !cancelled { onEnd(result) }
        }
    }

    /// Upload one attachment → what the daemon kept. One retry at eve's new address, as a turn has.
    func upload(data: Data, name: String, mime: String) async -> Result<ChatUploaded, ChatFailure> {
        var retried = false
        while true {
            guard let ep = endpoint() else {
                return .failure(ChatFailure(message: "Eve Link isn't paired.", addressMayHaveMoved: false))
            }
            let result = await Self.post(ChatRequests.upload(ep, data: data, name: name, mime: mime), session: session)
            if case .failure(let f) = result, f.addressMayHaveMoved, !retried, await refresh() {
                retried = true
                continue
            }
            return result.flatMap { body in
                ChatUploaded.decode(body).map { .success($0) }
                    ?? .failure(ChatFailure(message: "eve didn't keep that file.", addressMayHaveMoved: false))
            }
        }
    }

    /// One attachment's bytes and type, full size (GET /api/link/file/<id>). One retry at eve's new address.
    func download(id: String) async -> Result<ChatDownload, ChatFailure> {
        var retried = false
        while true {
            guard let ep = endpoint() else {
                return .failure(ChatFailure(message: "Eve Link isn't paired.", addressMayHaveMoved: false))
            }
            guard let request = ChatRequests.file(ep, id: id) else {
                return .failure(ChatFailure(message: "That file can't be opened.", addressMayHaveMoved: false))
            }
            let result = await Self.get(request, session: session)
            if case .failure(let f) = result, f.addressMayHaveMoved, !retried, await refresh() {
                retried = true
                continue
            }
            return result
        }
    }

    @concurrent
    private static func get(_ request: URLRequest, session: URLSession) async -> Result<ChatDownload, ChatFailure> {
        do {
            let (data, response) = try await session.data(for: request)
            let http = response as? HTTPURLResponse
            let status = http?.statusCode ?? 0
            if status == 404 { return .failure(ChatDownload.gone) }
            guard status == 200 else { return .failure(ChatFailure.from(status: status, body: data)) }
            return .success(ChatDownload(data: data, mime: http?.value(forHTTPHeaderField: "Content-Type") ?? ""))
        } catch {
            return .failure(ChatFailure.from(error))
        }
    }

    @concurrent
    private static func post(_ request: URLRequest, session: URLSession) async -> Result<Data, ChatFailure> {
        do {
            let (data, response) = try await session.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            guard status == 200 else { return .failure(ChatFailure.from(status: status, body: data)) }
            return .success(data)
        } catch {
            return .failure(ChatFailure.from(error))
        }
    }

    /// Stop a turn or a poll. `as` is what the page hears for a turn: nothing when it asked, onDone for the native
    /// Stop button, onError when the app gave up on it in the background.
    func cancel(id: String, as ending: ChatEnding = .silent) {
        guard let task = tasks[id] else { return }
        cancelledAs[id] = ending
        task.cancel()
    }

    func cancelAll(as ending: ChatEnding) {
        for id in tasks.keys { cancel(id: id, as: ending) }
    }

    func cancelTurns(as ending: ChatEnding) {
        for id in turnsInFlight { cancel(id: id, as: ending) }
    }

    /// One streamed request; nil when it ran to the end.
    @concurrent
    private static func stream(_ request: URLRequest, session: URLSession, into f: ByteForwarder) async -> ChatFailure? {
        do {
            let (bytes, response) = try await session.bytes(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            guard status == 200 else {
                var body: [UInt8] = []
                for try await b in bytes {
                    body.append(b)
                    if body.count >= 4096 { break }
                }
                return ChatFailure.from(status: status, body: Data(body))
            }
            try await ByteForwarder.pump(bytes, into: f)
            return nil
        } catch {
            return ChatFailure.from(error)
        }
    }

    @concurrent
    private static func fetch(_ request: URLRequest, session: URLSession) async -> Result<String, ChatFailure> {
        do {
            let (data, response) = try await session.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            guard status == 200 else { return .failure(ChatFailure.from(status: status, body: data)) }
            return .success(String(decoding: data, as: UTF8.self))
        } catch {
            return .failure(ChatFailure.from(error))
        }
    }
}

/// The diagnostics log in the app; a print in the Mac tests.
@MainActor
func diagChat(_ line: String) {
    #if os(iOS)
    diag(line)
    #else
    print(line)
    #endif
}
