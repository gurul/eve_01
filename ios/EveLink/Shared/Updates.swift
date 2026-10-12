// The Updates pane's data (docs/updates.md): things to act on, found by eve in the owner's mail, calendar, notes, memory
// and projects, read from the daemon through the tunnel with the link token (the same calls as WorkClient.swift). A
// tapped card sends its own visible sentence to the chat, and nothing else: email text is untrusted, so the owner sees
// on the card everything that gets sent. Foundation only, so Tests/main.swift checks the decoding, the requests, the
// tap text and the list's dismiss bookkeeping on the Mac.
//
//   GET  /api/link/updates            {"cards": [{"id", "verb", "text", "entity", "kind", "source", "due"}],
//                                      "generated_at": ISO | null, "refreshing": bool}
//   POST /api/link/updates/dismiss    {"id"} -> {"ok": true}: the card's sources stay out for 30 days;
//                                     404 {"ok": false} for an id the daemon does not know (the card comes back)
//   POST /api/link/updates/refresh    {} -> {"ok": true, "started": bool}: a rebuild in the background, never waited on
import Foundation

/// Where a card came from: the first source it cites.
enum UpdateKind: String, CaseIterable, Sendable {
    case email, calendar, note, memory, chief

    /// The word the tap text uses: "(from <label>: <source>)".
    var label: String {
        switch self {
        case .email: "email"
        case .calendar: "calendar"
        case .note: "notes"
        case .memory: "memory"
        case .chief: "projects"
        }
    }

    /// The SF Symbol on the card.
    var symbol: String {
        switch self {
        case .email: "envelope"
        case .calendar: "calendar"
        case .note: "note.text"
        case .memory: "brain"
        case .chief: "folder"
        }
    }
}

/// One thing to act on: a verb chip ("Review") and the rest of the sentence.
struct UpdateCard: Codable, Equatable, Sendable, Identifiable {
    var id: String
    var verb: String
    var text: String
    var entity: String?
    /// The raw kind as the daemon sent it; an unknown one is kept and shown with a generic symbol.
    var kind: String
    var source: String
    /// "YYYY-MM-DD", or nil.
    var due: String?

    /// The longest of each field the app shows and sends. The daemon clamps tighter; these keep a wrong answer from
    /// filling the screen, and what is shown is exactly what is sent.
    static let maxVerb = 24
    static let maxText = 160
    static let maxEntity = 40
    static let maxSource = 60

    var knownKind: UpdateKind? { UpdateKind(rawValue: kind) }

    init(id: String, verb: String, text: String, entity: String? = nil, kind: String, source: String = "",
         due: String? = nil) {
        self.id = id
        self.verb = Self.clean(verb, Self.maxVerb)
        self.text = Self.clean(text, Self.maxText)
        self.entity = entity.map { Self.clean($0, Self.maxEntity) }.flatMap { $0.isEmpty ? nil : $0 }
        self.kind = Self.clean(kind, 20).lowercased()
        self.source = Self.clean(source, Self.maxSource)
        self.due = due.flatMap { UpdateDue.parse($0) == nil ? nil : $0 }
    }

    private enum CodingKeys: String, CodingKey { case id, verb, text, entity, kind, source, due }

    /// A card without an id, a verb or a text does not decode (the list skips it); everything else is tolerated.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let id = try c.decode(String.self, forKey: .id)
        let verb = try c.decode(String.self, forKey: .verb)
        let text = try c.decode(String.self, forKey: .text)
        self.init(id: id, verb: verb, text: text,
                  entity: try? c.decodeIfPresent(String.self, forKey: .entity),
                  kind: (try? c.decodeIfPresent(String.self, forKey: .kind)) ?? "",
                  source: (try? c.decodeIfPresent(String.self, forKey: .source)) ?? "",
                  due: try? c.decodeIfPresent(String.self, forKey: .due))
        guard !self.id.isEmpty, !self.verb.isEmpty, !self.text.isEmpty else {
            throw DecodingError.dataCorruptedError(forKey: .id, in: c, debugDescription: "a card needs an id, a verb and a text")
        }
    }

    /// One line, whitespace (newlines included) collapsed to single spaces, at most `limit` characters.
    static func clean(_ s: String, _ limit: Int) -> String {
        let words = s.split(whereSeparator: { $0.isWhitespace || $0.isNewline })
        return String(words.joined(separator: " ").prefix(limit)).trimmingCharacters(in: .whitespaces)
    }
}

/// The GET's answer.
struct UpdatesFeed: Codable, Equatable, Sendable {
    var cards: [UpdateCard]
    var generatedAt: String?
    /// The daemon is rebuilding the cards in the background: ask again in a moment.
    var refreshing: Bool

    init(cards: [UpdateCard], generatedAt: String? = nil, refreshing: Bool = false) {
        self.cards = cards; self.generatedAt = generatedAt; self.refreshing = refreshing
    }

    private enum CodingKeys: String, CodingKey {
        case cards, refreshing
        case generatedAt = "generated_at"
    }

    /// A card that does not decode is left out; the rest of the list still shows. Duplicated ids keep the first.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        var list = try c.nestedUnkeyedContainer(forKey: .cards)
        var cards: [UpdateCard] = []
        var seen = Set<String>()
        while !list.isAtEnd {
            let before = list.currentIndex
            if (try? list.decodeNil()) == true { continue }
            if let card = try? list.decode(UpdateCard.self) {
                if seen.insert(card.id).inserted { cards.append(card) }
            } else {
                _ = try? list.decode(Skip.self)       // a failed decode does not move on; this one does
            }
            if list.currentIndex == before { break }  // never spin on an element nothing can read
        }
        self.cards = cards
        generatedAt = try? c.decodeIfPresent(String.self, forKey: .generatedAt)
        refreshing = (try? c.decodeIfPresent(Bool.self, forKey: .refreshing)) ?? false
    }

    /// Reads any one value and keeps nothing.
    private struct Skip: Decodable { init(from decoder: Decoder) throws {} }

    static func decode(_ data: Data) -> UpdatesFeed? { try? JSONDecoder().decode(UpdatesFeed.self, from: data) }
}

/// The answer to a dismiss or a refresh.
struct UpdatesAck: Equatable, Sendable {
    var ok: Bool
    var started: Bool

    static func decode(_ data: Data) -> UpdatesAck? {
        guard let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return nil }
        return UpdatesAck(ok: obj["ok"] as? Bool ?? false, started: obj["started"] as? Bool ?? false)
    }
}

/// What a tapped card sends into the chat, exactly: "<verb> <text>", and on a second line where it came from. No hidden
/// prompt: every word here is on the card.
enum UpdateTap {
    static func text(_ card: UpdateCard) -> String {
        let sentence = "\(card.verb) \(card.text)"
        guard !card.source.isEmpty else { return sentence }
        let from = card.knownKind.map { "\($0.label): \(card.source)" } ?? card.source
        return sentence + "\n(from \(from))"
    }
}

/// The small due label: "Today", "Tomorrow", "Oct 23", or "Overdue" for a day gone by.
enum UpdateDue {
    static func parse(_ s: String) -> DateComponents? {
        let parts = s.split(separator: "-")
        guard s.count == 10, parts.count == 3, let y = Int(parts[0]), let m = Int(parts[1]), let d = Int(parts[2]),
              (1...12).contains(m), (1...31).contains(d) else { return nil }
        return DateComponents(year: y, month: m, day: d)
    }

    /// `calendar`'s time zone decides what "today" is (the phone's own by default).
    static func label(_ due: String?, now: Date = Date(), calendar: Calendar = .current,
                      locale: Locale = .current) -> String? {
        guard let due, let parts = parse(due), let day = calendar.date(from: parts) else { return nil }
        let today = calendar.startOfDay(for: now)
        let days = calendar.dateComponents([.day], from: today, to: calendar.startOfDay(for: day)).day ?? 0
        switch days {
        case ..<0: return "Overdue"
        case 0: return "Today"
        case 1: return "Tomorrow"
        default:
            var style = Date.FormatStyle(date: .omitted, time: .omitted, locale: locale, calendar: calendar,
                                         timeZone: calendar.timeZone).month(.abbreviated).day()
            if calendar.component(.year, from: day) != calendar.component(.year, from: now) { style = style.year() }
            return day.formatted(style)
        }
    }

    /// Today or earlier: the label is drawn in the warning color.
    static func isPressing(_ due: String?, now: Date = Date(), calendar: Calendar = .current) -> Bool {
        guard let due, let parts = parse(due), let day = calendar.date(from: parts) else { return false }
        return calendar.startOfDay(for: day) <= calendar.startOfDay(for: now)
    }
}

/// The list as the pane shows it, with the bookkeeping that keeps it honest when answers cross: a card dismissed here
/// stays gone even if a read that started before the dismiss lands after it, an older read never overwrites a newer
/// one, and a dismiss that fails puts the card back where it was.
struct UpdatesList: Equatable, Sendable {
    private(set) var cards: [UpdateCard]?
    private(set) var generatedAt: String?
    private(set) var refreshing = false
    /// Dismissed from this phone, in flight or done: never shown again this session (the daemon drops their sources).
    private(set) var dismissed = Set<String>()
    private var lastApplied = 0
    private var issued = 0

    /// A number for a read about to start; pass it back to `apply`.
    mutating func nextRead() -> Int {
        issued += 1
        return issued
    }

    /// A read's answer. false when a newer read already landed and this one was ignored.
    @discardableResult
    mutating func apply(_ feed: UpdatesFeed, read: Int) -> Bool {
        guard read > lastApplied else { return false }
        lastApplied = read
        cards = feed.cards.filter { !dismissed.contains($0.id) }
        generatedAt = feed.generatedAt
        refreshing = feed.refreshing
        return true
    }

    /// Takes the card off the list at once. The card and where it was, for `restore` if the daemon refuses.
    mutating func dismiss(_ id: String) -> (card: UpdateCard, index: Int)? {
        guard let i = cards?.firstIndex(where: { $0.id == id }), let card = cards?.remove(at: i) else { return nil }
        dismissed.insert(id)
        return (card, i)
    }

    /// The dismiss failed: the card comes back at its place (or the end, if the list has shrunk since).
    mutating func restore(_ card: UpdateCard, at index: Int) {
        dismissed.remove(card.id)
        guard var list = cards, !list.contains(where: { $0.id == card.id }) else { return }
        list.insert(card, at: min(max(index, 0), list.count))
        cards = list
    }
}

enum UpdatesRequests {
    static func list(_ ep: ChatEndpoint) -> URLRequest { WorkRequests.request(ep, ["api", "link", "updates"], post: false) }
    static func dismiss(_ ep: ChatEndpoint, id: String) -> URLRequest {
        WorkRequests.request(ep, ["api", "link", "updates", "dismiss"], post: true, body: ["id": id])
    }
    static func refresh(_ ep: ChatEndpoint) -> URLRequest {
        WorkRequests.request(ep, ["api", "link", "updates", "refresh"], post: true)
    }
}

extension WorkClient {
    func updates() async -> Result<UpdatesFeed, ChatFailure> {
        await call(.updates, UpdatesRequests.list) { data, _ in UpdatesFeed.decode(data) }
    }

    func dismissUpdate(_ id: String) async -> Result<UpdatesAck, ChatFailure> {
        await call(.updates, { UpdatesRequests.dismiss($0, id: id) }) { data, _ in UpdatesAck.decode(data) }
    }

    func refreshUpdates() async -> Result<UpdatesAck, ChatFailure> {
        await call(.updates, UpdatesRequests.refresh) { data, _ in UpdatesAck.decode(data) }
    }
}
