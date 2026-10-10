// The home-screen widget's data (docs/widget.md): one read of the daemon's GET /api/link/today with the link token,
// the last good answer kept in the widget's own container, and which rows fit. Foundation only, so Tests/main.swift
// checks the decoding, the fallback and the layout on the Mac.
//
//   GET /api/link/today   {"at", "needs_you", "needs": [row], "going": [row], "today": [event], "now": [row],
//                          "sources": {...}}; row = {"id", "kind", "title", "line"},
//                          event = {"title", "start", "end", "all_day", "place"?} (unix seconds)
import Foundation

enum TodayWidgetKind {
    static let kind = "com.github.eve-bridge.EveLink.today"
    static let workURL = URL(string: "evelink://work")!
}

/// One line on the widget: a job (needs you or going), a watch, or what plays now.
struct TodayRow: Codable, Equatable, Sendable {
    var id: String
    var kind: String            // job | watch | music | tv
    var title: String
    var line: String

    init(id: String = "", kind: String, title: String, line: String = "") {
        self.id = id; self.kind = kind; self.title = title; self.line = line
    }

    init?(json: Any) {
        guard let o = json as? [String: Any], let title = o["title"] as? String, !title.isEmpty else { return nil }
        self.init(id: o["id"] as? String ?? "", kind: o["kind"] as? String ?? "", title: title,
                  line: o["line"] as? String ?? "")
    }
}

/// One event left today on the owner's calendar.
struct TodayEvent: Codable, Equatable, Sendable {
    var title: String
    var start: Date
    var end: Date
    var allDay: Bool

    init(title: String, start: Date, end: Date, allDay: Bool = false) {
        self.title = title; self.start = start; self.end = end; self.allDay = allDay
    }

    init?(json: Any) {
        guard let o = json as? [String: Any], let title = o["title"] as? String,
              let s = (o["start"] as? NSNumber)?.doubleValue else { return nil }
        let e = (o["end"] as? NSNumber)?.doubleValue ?? s + 3600
        self.init(title: title, start: Date(timeIntervalSince1970: s), end: Date(timeIntervalSince1970: e),
                  allDay: o["all_day"] as? Bool ?? false)
    }
}

struct TodayFeed: Codable, Equatable, Sendable {
    var at: Date
    var needsYou: Int
    var needs: [TodayRow]
    var going: [TodayRow]
    var events: [TodayEvent]
    var now: [TodayRow]

    static func decode(_ data: Data) -> TodayFeed? {
        guard let o = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let at = (o["at"] as? NSNumber)?.doubleValue else { return nil }
        func rows(_ key: String) -> [TodayRow] { (o[key] as? [Any] ?? []).compactMap(TodayRow.init(json:)) }
        let needs = rows("needs")
        return TodayFeed(at: Date(timeIntervalSince1970: at),
                         needsYou: max((o["needs_you"] as? NSNumber)?.intValue ?? needs.count, needs.count),
                         needs: needs, going: rows("going"),
                         events: (o["today"] as? [Any] ?? []).compactMap(TodayEvent.init(json:)), now: rows("now"))
    }

    /// The events not over at `date` (an entry drawn later than the read drops what ended since).
    func events(at date: Date) -> [TodayEvent] { events.filter { $0.end > date } }

    func isEmpty(at date: Date) -> Bool { needs.isEmpty && going.isEmpty && events(at: date).isEmpty && now.isEmpty }
}

/// Why the widget shows old data, or none.
enum TodayProblem: String, Codable, Equatable, Sendable {
    case unpaired       // no pairing in the shared Keychain group
    case unreachable    // the Mac or the tunnel did not answer
    case outdated       // this eve has no /api/link/today yet
    case refused        // the link token was refused: pair again

    var line: String {
        switch self {
        case .unpaired: "Open Eve Link to pair."
        case .unreachable: "Can't reach the Mac."
        case .outdated: "Update eve on the Mac."
        case .refused: "Pair Eve Link again."
        }
    }
}

/// What one refresh found: fresh data, or the last good data and why it is not fresh.
struct TodaySnapshot: Equatable, Sendable {
    var feed: TodayFeed?
    var fetched: Date?          // when `feed` was read
    var problem: TodayProblem?

    var stale: Bool { problem != nil && feed != nil }
}

/// The last good answer, in the widget extension's own container (only the widget reads it).
struct TodayCache: Sendable {
    let url: URL?

    static let standard = TodayCache(url: FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
        .first?.appendingPathComponent("EveLinkToday.json"))

    private struct Kept: Codable {
        var feed: TodayFeed
        var fetched: Date
    }

    func load() -> (TodayFeed, Date)? {
        guard let url, let data = try? Data(contentsOf: url),
              let kept = try? JSONDecoder().decode(Kept.self, from: data) else { return nil }
        return (kept.feed, kept.fetched)
    }

    func save(_ feed: TodayFeed, fetched: Date) {
        guard let url, let data = try? JSONEncoder().encode(Kept(feed: feed, fetched: fetched)) else { return }
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }
}

enum TodayLoader {
    typealias Fetch = @Sendable (URLRequest) async throws -> (Data, Int)

    static func request(_ p: Pairing) -> URLRequest {
        var r = URLRequest(url: p.base.appending(components: "api", "link", "today"))
        r.setValue("Bearer \(p.token)", forHTTPHeaderField: "Authorization")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        r.timeoutInterval = 15
        r.cachePolicy = .reloadIgnoringLocalCacheData
        return r
    }

    /// The tunnel is gone (Cloudflare's 502 or 530, or no answer at all): eve may be at a new address.
    static func moved(_ status: Int) -> Bool { status == 502 || status == 530 || status == 0 }

    /// One refresh. A failure to reach eve asks the rendezvous (Rendezvous.swift) for its new address and tries once
    /// more there; `repaired` keeps a new address in the Keychain. Any failure falls back to the cache.
    static func load(pairing: Pairing?, cache: TodayCache, now: Date = Date(), fetch: Fetch,
                     rendezvous: @Sendable (String) async -> URL?, repaired: @Sendable (Pairing) -> Void) async
        -> TodaySnapshot {
        guard var pairing else { return TodaySnapshot(feed: nil, fetched: nil, problem: .unpaired) }
        var problem = TodayProblem.unreachable
        var retried = false
        while true {
            var status = 0                          // 0: no answer at all
            var body = Data()
            if let got = try? await fetch(request(pairing)) { (body, status) = got }
            if status == 200, let feed = TodayFeed.decode(body) {
                cache.save(feed, fetched: now)
                return TodaySnapshot(feed: feed, fetched: now, problem: nil)
            }
            switch status {
            case 401: problem = .refused
            case 404: problem = .outdated
            default: problem = .unreachable
            }
            if moved(status), !retried, let fresh = await rendezvous(pairing.token), fresh != pairing.base {
                retried = true
                pairing.base = fresh
                repaired(pairing)
                continue
            }
            break
        }
        let kept = cache.load()
        return TodaySnapshot(feed: kept?.0, fetched: kept?.1, problem: problem)
    }
}

/// Which rows the widget draws. Sections in the owner's order (Needs you, Going, Today, Now), each shown only with
/// content; when they do not all fit, each keeps a heading and its first row, and the rest are filled in that same
/// priority. A section cut short says how many more it has.
struct TodayLine: Equatable, Sendable {
    enum Lead: Equatable, Sendable {
        case symbol(String)
        case time(Date)
        case now            // an event under way
    }
    var lead: Lead
    var title: String
    var detail: String
}

struct TodaySection: Equatable, Sendable {
    enum Kind: String, Sendable { case needs, going, today, now }
    var kind: Kind
    var lines: [TodayLine]
    var count: Int          // every row it has, shown or not

    var title: String {
        switch kind {
        case .needs: "Needs you"
        case .going: "Going"
        case .today: "Today"
        case .now: "Now"
        }
    }
    var more: Int { max(count - lines.count, 0) }
}

enum TodayLayout {
    static func symbol(_ row: TodayRow, needs: Bool) -> String {
        if needs { return "hand.raised" }
        switch row.kind {
        case "watch": return "eye"
        case "music": return "music.note"
        case "tv": return "tv"
        default: return "circle.dashed"
        }
    }

    static func lines(_ kind: TodaySection.Kind, _ feed: TodayFeed, at date: Date) -> [TodayLine] {
        switch kind {
        case .needs: feed.needs.map { TodayLine(lead: .symbol(symbol($0, needs: true)), title: $0.title, detail: $0.line) }
        case .going: feed.going.map { TodayLine(lead: .symbol(symbol($0, needs: false)), title: $0.title, detail: $0.line) }
        case .now: feed.now.map { TodayLine(lead: .symbol(symbol($0, needs: false)), title: $0.title, detail: $0.line) }
        case .today:
            feed.events(at: date).map { e in
                TodayLine(lead: e.allDay ? .symbol("calendar") : e.start <= date ? .now : .time(e.start),
                          title: e.title, detail: "")
            }
        }
    }

    /// `budget`: lines the widget has room for, a heading counting as one.
    static func sections(_ feed: TodayFeed, at date: Date, budget: Int) -> [TodaySection] {
        let order: [TodaySection.Kind] = [.needs, .going, .today, .now]
        var all = order.map { kind -> TodaySection in
            let l = lines(kind, feed, at: date)
            return TodaySection(kind: kind, lines: l, count: kind == .needs ? max(feed.needsYou, l.count) : l.count)
        }.filter { !$0.lines.isEmpty }
        var shown = all.map { _ in 0 }
        var left = budget
        // the priority for room: what needs the owner, then what is next on the calendar, then the rest
        let priority = [TodaySection.Kind.needs, .today, .going, .now].compactMap { k in all.firstIndex { $0.kind == k } }
        for i in priority where left >= 2 {
            shown[i] = 1
            left -= 2
        }
        for i in priority {
            let add = min(all[i].lines.count - shown[i], left)
            if shown[i] > 0, add > 0 {
                shown[i] += add
                left -= add
            }
        }
        for i in all.indices { all[i].lines = Array(all[i].lines.prefix(shown[i])) }
        return all.filter { !$0.lines.isEmpty }
    }

    /// The medium widget: no headings, the rows in priority order.
    static func flat(_ feed: TodayFeed, at date: Date, rows: Int) -> [TodayLine] {
        Array(([.needs, .today, .going, .now] as [TodaySection.Kind]).flatMap { lines($0, feed, at: date) }.prefix(rows))
    }
}
