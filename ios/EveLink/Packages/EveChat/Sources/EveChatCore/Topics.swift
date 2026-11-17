// Horizontal History (docs/app-chat.md): eve's one conversation as topic cards. The daemon cuts the transcript into
// topics at pauses of more than 45 minutes (link_topics.py); this file only shapes, orders and dates them, and finds
// where a topic starts in the loaded chat.
import Foundation

public struct ChatTopic: Identifiable, Equatable, Sendable {
    public var id: String
    public var firstID: String
    public var lastID: String
    public var startedAt: Double
    public var endedAt: Double
    public var count: Int
    public var title: String
    public var preview: String
    public var bookmarked: Bool

    public init(id: String, firstID: String, lastID: String, startedAt: Double = 0, endedAt: Double = 0, count: Int = 1,
                title: String, preview: String = "", bookmarked: Bool = false) {
        self.id = id
        self.firstID = firstID
        self.lastID = lastID
        self.startedAt = startedAt
        self.endedAt = endedAt
        self.count = count
        self.title = title
        self.preview = preview
        self.bookmarked = bookmarked
    }

    /// `<day>:<line index>` as (day, index), or nil.
    static func key(_ id: String) -> (day: String, index: Int)? {
        guard id.range(of: #"^\d{4}-\d{2}-\d{2}:\d{1,7}$"#, options: .regularExpression) != nil,
              let colon = id.firstIndex(of: ":"), let index = Int(id[id.index(after: colon)...]) else { return nil }
        return (String(id[..<colon]), index)
    }

    init?(json: Any) {
        guard let r = json as? [String: Any] else { return nil }
        func str(_ k: String) -> String { (r[k] as? String) ?? "" }
        func num(_ k: String) -> Double {
            guard let n = r[k] as? NSNumber, !(r[k] is Bool) else { return 0 }
            return n.doubleValue.isFinite ? n.doubleValue : 0
        }
        guard Self.key(str("id")) != nil, Self.key(str("first_id")) != nil, Self.key(str("last_id")) != nil,
              !str("title").isEmpty else { return nil }
        self.init(id: str("id"), firstID: str("first_id"), lastID: str("last_id"), startedAt: num("started_at"),
                  endedAt: num("ended_at"), count: max(1, Int(num("count").rounded())), title: str("title"),
                  preview: str("preview"), bookmarked: (r["bookmarked"] as? Bool) == true)
    }

    /// Whether a message id falls inside the topic (between its first and last message).
    public func contains(_ messageID: String) -> Bool {
        guard let k = Self.key(messageID), let lo = Self.key(firstID), let hi = Self.key(lastID) else { return false }
        func atOrAfter(_ a: (day: String, index: Int), _ b: (day: String, index: Int)) -> Bool {
            a.day == b.day ? a.index >= b.index : a.day > b.day
        }
        return atOrAfter(k, lo) && atOrAfter(hi, k)
    }

    /// The first loaded message id of the topic, in chat order, or nil when none of it is loaded.
    public func start(in loaded: [String]) -> String? { loaded.first(where: contains) }

    /// What the owner sends to pick the topic back up when it is no longer loaded.
    public var pickUp: String { "Let's pick up \"\(title)\" again." }
}

public struct ChatTopics: Equatable, Sendable {
    public var topics: [ChatTopic]
    public var bookmarked: [ChatTopic]

    public init(topics: [ChatTopic] = [], bookmarked: [ChatTopic] = []) {
        self.topics = topics
        self.bookmarked = bookmarked
    }

    /// The daemon's answer, checked: anything malformed is dropped, never shown.
    public static func parse(_ json: String) -> ChatTopics {
        let r = (try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any]) ?? [:]
        func list(_ k: String) -> [ChatTopic] { ((r[k] as? [Any]) ?? []).compactMap(ChatTopic.init(json:)) }
        return ChatTopics(topics: list("topics"), bookmarked: list("bookmarked"))
    }

    /// Saved first, then the recent ones that are not saved: each topic once.
    public var cards: [ChatTopic] {
        let saved = Set(bookmarked.map(\.id))
        return bookmarked + topics.filter { !saved.contains($0.id) }
    }

    /// Set a topic's star in both lists (shown at once, and put back if the Mac refuses).
    public func withBookmark(_ id: String, on: Bool) -> ChatTopics {
        let marked = topics.map { t -> ChatTopic in
            var t = t
            if t.id == id { t.bookmarked = on }
            return t
        }
        let found = marked.first { $0.id == id } ?? bookmarked.first { $0.id == id }
        let rest = bookmarked.filter { $0.id != id }
        guard on, var first = found else { return ChatTopics(topics: marked, bookmarked: rest) }
        first.bookmarked = true
        return ChatTopics(topics: marked, bookmarked: [first] + rest)
    }

    /// "now", "12 min", "3 h", "Tue", "Sep 28": short, like a phone's message list.
    public static func when(_ at: Double, now: Double = Date().timeIntervalSince1970, calendar: Calendar = .current,
                            locale: Locale = .current) -> String {
        let s = max(0, now - at)
        if s < 90 { return "now" }
        if s < 3600 { return "\(Int((s / 60).rounded())) min" }
        if s < 20 * 3600 { return "\(Int((s / 3600).rounded())) h" }
        let f = DateFormatter()
        f.calendar = calendar
        f.locale = locale
        f.timeZone = calendar.timeZone
        f.setLocalizedDateFormatFromTemplate(s < 6 * 86400 ? "EEE" : "MMMd")
        return f.string(from: Date(timeIntervalSince1970: at))
    }
}
