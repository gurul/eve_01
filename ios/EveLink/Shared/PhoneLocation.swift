// The phone's live location for eve (owner, 2026-10-12: "give access to live phone location through app"). The app
// (App/LocationSharer.swift) sends the latest fix to the Mac, which keeps only that one fix and never sends it on; eve
// reads it with its phone_location tool. Foundation only, so Tests/main.swift checks the body, the throttle and the
// replies on the Mac.
//
//   POST /api/link/location   {"lat", "lon", "accuracy_m", "at": ISO 8601, "place": str | null, "speed_mps": num | null}
//                             -> {"ok": true}; 400 for a fix the Mac refuses, 403 while EVE_PHONE_LOCATION=0
//
// Places (owner, 2026-10-18: "make a location service that tracks location ... patterns habits where things are,
// cuz rn u get pure coordinates"): the Mac now keeps the fixes as history and turns them into places, visits and
// routines (bridge places.py). With Always, iOS also tells the app when the owner arrives at and leaves a place
// (CLVisit, the cheapest signal iOS has for it), and the app forwards each one:
//
//   POST /api/link/visit      {"lat", "lon", "accuracy_m", "arrived": ISO 8601 | null, "departed": ISO 8601 | null,
//                              "place": str | null, "poi": str | null, "category": str | null}
//                             -> {"ok": true}; 403 while places are off on the Mac, 404 from an older Mac (dropped)
//   GET  /api/link/places     -> {"ok": true, "on", "places", "visits", "things", "since": "YYYY-MM-DD" | null}
//   POST /api/link/places/forget  {} -> {"ok": true, "forgot": {...}}: the history, places, things and latest fix
//
// Nothing here logs a coordinate or a place: a diagnostics line says how accurate and how old a fix was, never where.
import Foundation

/// One fix from the phone, checked: finite, on the globe, with a real accuracy. Built from a CLLocation's fields in
/// the app; nil for the invalid fixes Core Location can hand out (a negative accuracy means "no position").
struct LocationFix: Equatable, Sendable {
    var lat: Double
    var lon: Double
    var accuracyM: Double
    var at: Date
    var speedMps: Double?

    /// The widest circle sent: a coarser fix than this says nothing about where the phone is.
    static let maxAccuracyM = 50_000.0

    init?(lat: Double, lon: Double, accuracyM: Double, at: Date, speedMps: Double? = nil) {
        guard lat.isFinite, lon.isFinite, (-90...90).contains(lat), (-180...180).contains(lon),
              accuracyM.isFinite, accuracyM > 0, accuracyM <= Self.maxAccuracyM else { return nil }
        self.lat = lat
        self.lon = lon
        self.accuracyM = accuracyM
        self.at = at
        // Core Location says -1 for "no speed"; a speed above ~340 m/s (an airliner is ~250) is not a phone's.
        self.speedMps = speedMps.flatMap { $0.isFinite && $0 >= 0 && $0 < 340 ? $0 : nil }
    }

    /// Metres between two points on the Earth (haversine, mean radius): plenty for 100 m and 200 m thresholds.
    static func distance(_ a: (lat: Double, lon: Double), _ b: (lat: Double, lon: Double)) -> Double {
        let r = 6_371_008.8
        let p1 = a.lat * .pi / 180, p2 = b.lat * .pi / 180
        let dp = (b.lat - a.lat) * .pi / 180, dl = (b.lon - a.lon) * .pi / 180
        let h = sin(dp / 2) * sin(dp / 2) + cos(p1) * cos(p2) * sin(dl / 2) * sin(dl / 2)
        return 2 * r * asin(min(1, sqrt(h)))
    }

    func distance(to other: LocationFix) -> Double { Self.distance((lat, lon), (other.lat, other.lon)) }
}

/// The request body, exactly as the contract above names it. `place` and `speed_mps` are always present, as null when
/// there is none.
struct PhoneLocationBody: Codable, Equatable, Sendable {
    var lat: Double
    var lon: Double
    var accuracyM: Double
    var at: String
    var place: String?
    var speedMps: Double?

    enum CodingKeys: String, CodingKey {
        case lat, lon, place, at
        case accuracyM = "accuracy_m"
        case speedMps = "speed_mps"
    }

    init(_ fix: LocationFix, place: String?) {
        // 6 decimals is ~0.1 m: as precise as any phone fix, without float noise on the wire.
        lat = (fix.lat * 1e6).rounded() / 1e6
        lon = (fix.lon * 1e6).rounded() / 1e6
        accuracyM = (fix.accuracyM * 10).rounded() / 10
        at = Self.stamp(fix.at)
        let trimmed = place?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.place = trimmed.isEmpty ? nil : String(trimmed.prefix(PlaceRule.maxLength))
        speedMps = fix.speedMps.map { ($0 * 10).rounded() / 10 }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(lat, forKey: .lat)
        try c.encode(lon, forKey: .lon)
        try c.encode(accuracyM, forKey: .accuracyM)
        try c.encode(at, forKey: .at)
        try c.encode(place, forKey: .place)          // null, not left out
        try c.encode(speedMps, forKey: .speedMps)
    }

    /// UTC with a Z, whole seconds: "2026-10-12T17:03:05Z" (Python's datetime.fromisoformat reads it).
    static func stamp(_ date: Date) -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        f.timeZone = TimeZone(identifier: "UTC")
        return f.string(from: date)
    }

    func json() -> Data {
        let e = JSONEncoder()
        e.outputFormatting = [.sortedKeys]
        return (try? e.encode(self)) ?? Data("{}".utf8)
    }
}

/// When a new fix is worth sending. The Mac keeps one fix; each send is one small POST, but a send in the background
/// costs a radio wake, so the rule is about meaning, not freshness alone.
enum LocationThrottle {
    /// A move this far (and farther than the fix's own uncertainty) is a new place.
    static let minMove = 100.0
    /// The same place is re-sent after this long, so eve can tell the fix is current: 10 min with the app open,
    /// 30 min for a fix that woke the app in the background anyway (significant-change).
    static let refreshForeground: TimeInterval = 10 * 60
    static let refreshBackground: TimeInterval = 30 * 60
    /// Never two sends closer than this (Core Location bursts a few fixes as it settles).
    static let minGap: TimeInterval = 15
    /// A fix older than this when it arrives (Core Location's cached first fix) is not "live": not sent.
    static let maxAge: TimeInterval = 10 * 60

    enum Decision: Equatable, Sendable {
        case send(Reason)
        case skip(Skip)
    }
    enum Reason: String, Sendable { case first, moved, sharper, refresh }
    enum Skip: String, Sendable { case old, notNewer, tooSoon, same }

    /// `last`: the fix sent last (its own time, not when it was sent). `now`: the phone's clock.
    static func decide(_ fix: LocationFix, last: LocationFix?, now: Date, foreground: Bool) -> Decision {
        if now.timeIntervalSince(fix.at) > maxAge { return .skip(.old) }
        guard let last else { return .send(.first) }
        let gap = fix.at.timeIntervalSince(last.at)
        if gap <= 0 { return .skip(.notNewer) }          // the Mac ignores an older fix too
        if gap < minGap { return .skip(.tooSoon) }
        let moved = fix.distance(to: last)
        if moved >= max(minMove, fix.accuracyM) { return .send(.moved) }
        // "a lot" sharper: at most half the old circle and at least 50 m tighter (a cell fix, then GPS)
        if fix.accuracyM <= last.accuracyM / 2, last.accuracyM - fix.accuracyM >= 50 { return .send(.sharper) }
        if gap >= (foreground ? refreshForeground : refreshBackground) { return .send(.refresh) }
        return .skip(.same)
    }
}

/// The "place" line and its cache. The phone names the place (reverse geocoding), at most once a minute, and reuses
/// the last name while the phone is within 200 m of where it was named.
enum PlaceRule {
    static let reuseWithin = 200.0
    static let minLookupGap: TimeInterval = 60
    /// Street-level names only for a fix this sharp; a coarser one gets the city ("Springfield, IL").
    static let streetAccuracy = 100.0
    static let maxLength = 120

    struct Cached: Codable, Equatable, Sendable {
        var lat: Double
        var lon: Double
        var place: String
        var at: Date
        /// The fix's accuracy when it was named: a city name from a coarse fix is not reused for a sharp one.
        var accuracyM: Double
    }

    /// The cached name when it still fits this fix, else nil.
    static func reuse(_ cached: Cached?, for fix: LocationFix) -> String? {
        guard let cached else { return nil }
        if LocationFix.distance((cached.lat, cached.lon), (fix.lat, fix.lon)) > reuseWithin { return nil }
        // a city-level name for a now street-level fix: name it again when allowed
        if cached.accuracyM > streetAccuracy, fix.accuracyM <= streetAccuracy { return nil }
        return cached.place
    }

    static func mayLookUp(last: Date?, now: Date) -> Bool {
        guard let last else { return true }
        return now.timeIntervalSince(last) >= minLookupGap || now < last
    }

    /// The line from a reverse geocode: "<street-level name>, <city>" for a sharp fix, the city otherwise; nil when
    /// the geocoder named nothing.
    static func line(name: String?, city: String?, accuracyM: Double) -> String? {
        let n = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let c = city?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if accuracyM <= streetAccuracy, !n.isEmpty {
            if c.isEmpty || n == c || n.hasSuffix(c) { return String(n.prefix(maxLength)) }
            let cityOnly = c.split(separator: ",").first.map(String.init) ?? c
            if n.contains(cityOnly) { return String(n.prefix(maxLength)) }
            return String("\(n), \(c)".prefix(maxLength))
        }
        if !c.isEmpty { return String(c.prefix(maxLength)) }
        return nil
    }
}

/// What the Mac said to a send, in the words the Settings row shows.
enum LocationReply: Equatable, Sendable {
    case ok
    case offOnMac             // 403: EVE_PHONE_LOCATION=0 (or the Origin check, which the app never trips)
    case outdated             // 404: an eve without the route
    case refused              // 401: the pairing is out of date
    case rejected             // 400: the Mac refused this fix
    case unreachable          // the tunnel is gone: ask the rendezvous, try once more
    case failed(String)

    static func from(status: Int, body: Data) -> LocationReply {
        switch status {
        case 200:
            let obj = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
            return obj?["ok"] as? Bool == true ? .ok : .failed("eve's answer didn't make sense.")
        case 400: return .rejected
        case 401: return .refused
        case 403: return .offOnMac
        case 404: return .outdated
        case 502, 530: return .unreachable
        default: return .failed("eve answered \(status).")
        }
    }

    var addressMayHaveMoved: Bool { self == .unreachable }

    /// nil when all is well.
    var problem: String? {
        switch self {
        case .ok: nil
        case .offOnMac: "Location is off on the Mac (EVE_PHONE_LOCATION=0)."
        case .outdated: "This eve can't take the phone's location yet: update it on the Mac."
        case .refused: "Eve Link's pairing is out of date. Pair again."
        case .rejected: "eve didn't take the last fix."
        case .unreachable: "Can't reach eve."
        case .failed(let why): why
        }
    }
}

enum LocationRequests {
    static func post(_ ep: ChatEndpoint, _ body: PhoneLocationBody) -> URLRequest {
        var r = URLRequest(url: ep.base.appending(components: "api", "link", "location"))
        r.httpMethod = "POST"
        r.setValue("Bearer \(ep.token)", forHTTPHeaderField: "Authorization")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = body.json()
        r.timeoutInterval = 20
        r.cachePolicy = .reloadIgnoringLocalCacheData
        return r
    }
}

/// "Last sent" in Settings: "just now", "4 min ago", "2 h ago", "3 days ago"; "not yet" before the first.
enum LocationAge {
    static func words(since sent: Date?, now: Date) -> String {
        guard let sent else { return "Not yet" }
        let s = max(0, now.timeIntervalSince(sent))
        if s < 60 { return "Just now" }
        if s < 3600 { return "\(Int(s / 60)) min ago" }
        if s < 86_400 { return "\(Int(s / 3600)) h ago" }
        let d = Int(s / 86_400)
        return d == 1 ? "1 day ago" : "\(d) days ago"
    }
}

// ---- visits (CLVisit) ----------------------------------------------------------------------------------------------

/// One arrival or departure iOS reported, checked like a fix. iOS marks an unknown arrival with `distantPast` and a
/// visit still going on with `distantFuture`: both become nil here. A visit with neither time says nothing: nil.
struct VisitEvent: Equatable, Sendable {
    var lat: Double
    var lon: Double
    var accuracyM: Double
    var arrived: Date?
    var departed: Date?

    init?(lat: Double, lon: Double, accuracyM: Double, arrival: Date, departure: Date) {
        guard let fix = LocationFix(lat: lat, lon: lon, accuracyM: accuracyM, at: Date()) else { return nil }
        self.lat = fix.lat
        self.lon = fix.lon
        self.accuracyM = fix.accuracyM
        arrived = Self.real(arrival)
        departed = Self.real(departure)
        if arrived == nil, departed == nil { return nil }
        if let a = arrived, let d = departed, d < a { return nil }
    }

    /// A date iOS means (not one of its sentinels): after 2001 and before 2200.
    private static func real(_ d: Date) -> Date? {
        let t = d.timeIntervalSinceReferenceDate
        return t > 0 && t < 6_000_000_000 ? d : nil
    }

    /// The point as a fix, for naming the place (PlaceRule's cache and lookup take a fix).
    var fix: LocationFix? { LocationFix(lat: lat, lon: lon, accuracyM: accuracyM, at: departed ?? arrived ?? Date()) }
}

/// The visit's request body, as the contract above names it: every key present, null when unknown.
struct PhoneVisitBody: Codable, Equatable, Sendable {
    var lat: Double
    var lon: Double
    var accuracyM: Double
    var arrived: String?
    var departed: String?
    var place: String?
    var poi: String?
    var category: String?

    enum CodingKeys: String, CodingKey {
        case lat, lon, arrived, departed, place, poi, category
        case accuracyM = "accuracy_m"
    }

    init(_ visit: VisitEvent, place: String?, poi: String?, category: String?) {
        lat = (visit.lat * 1e6).rounded() / 1e6
        lon = (visit.lon * 1e6).rounded() / 1e6
        accuracyM = (visit.accuracyM * 10).rounded() / 10
        arrived = visit.arrived.map(PhoneLocationBody.stamp)
        departed = visit.departed.map(PhoneLocationBody.stamp)
        self.place = Self.clean(place)
        self.poi = Self.clean(poi)
        self.category = Self.clean(category).map { String($0.prefix(40)) }
    }

    private static func clean(_ s: String?) -> String? {
        let t = s?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return t.isEmpty ? nil : String(t.prefix(PlaceRule.maxLength))
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(lat, forKey: .lat)
        try c.encode(lon, forKey: .lon)
        try c.encode(accuracyM, forKey: .accuracyM)
        try c.encode(arrived, forKey: .arrived)      // null, not left out
        try c.encode(departed, forKey: .departed)
        try c.encode(place, forKey: .place)
        try c.encode(poi, forKey: .poi)
        try c.encode(category, forKey: .category)
    }

    func json() -> Data {
        let e = JSONEncoder()
        e.outputFormatting = [.sortedKeys]
        return (try? e.encode(self)) ?? Data("{}".utf8)
    }

    /// MapKit's category raw value ("MKPOICategoryFitnessCenter") in the contract's form ("fitnessCenter").
    static func category(_ raw: String?) -> String? {
        guard var s = raw?.trimmingCharacters(in: .whitespaces), !s.isEmpty else { return nil }
        if s.hasPrefix("MKPOICategory") { s.removeFirst("MKPOICategory".count) }
        guard let first = s.first else { return nil }
        return first.lowercased() + s.dropFirst()
    }
}

/// Visits that could not be sent yet (the Mac asleep, the tunnel gone), oldest first, kept on this phone until the next
/// send gets through. Capped: past `limit` the oldest go, so a week away never becomes an unbounded backlog.
struct VisitQueue: Codable, Equatable, Sendable {
    static let limit = 50
    private(set) var items: [PhoneVisitBody] = []

    mutating func add(_ body: PhoneVisitBody) {
        if let i = items.firstIndex(where: { $0.arrived != nil && $0.arrived == body.arrived
                                            && LocationFix.distance(($0.lat, $0.lon), (body.lat, body.lon)) < 200 }) {
            items[i] = body              // the departure replaces its own arrival: one visit, its latest word
        } else {
            items.append(body)
        }
        if items.count > Self.limit { items.removeFirst(items.count - Self.limit) }
    }

    mutating func dropFirst() { if !items.isEmpty { items.removeFirst() } }
    var first: PhoneVisitBody? { items.first }
    var isEmpty: Bool { items.isEmpty }
}

/// What a visit send means for the queue: sent and kept are plain; the rest decide whether to try again later.
enum VisitOutcome: Equatable, Sendable {
    case sent          // 200: off the queue
    case drop          // 400 (refused), 403 (off on the Mac), 404 (an older Mac): off the queue, never retried
    case retry         // anything else: stays, the next send tries again

    static func from(_ reply: LocationReply) -> VisitOutcome {
        switch reply {
        case .ok: .sent
        case .rejected, .offOnMac, .outdated: .drop
        case .refused, .unreachable, .failed: .retry
        }
    }
}

/// GET /api/link/places, for the Settings row.
struct PlacesSummary: Decodable, Equatable, Sendable {
    var on: Bool
    var places: Int
    var visits: Int
    var things: Int
    var since: String?

    static func from(status: Int, body: Data) -> PlacesSummary? {
        guard status == 200,
              let obj = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any],
              obj["ok"] as? Bool == true else { return nil }
        return PlacesSummary(on: obj["on"] as? Bool ?? true, places: obj["places"] as? Int ?? 0,
                             visits: obj["visits"] as? Int ?? 0, things: obj["things"] as? Int ?? 0,
                             since: obj["since"] as? String)
    }

    /// "12 places" / "1 place" / "None yet".
    var words: String {
        if places == 0 { return "None yet" }
        return places == 1 ? "1 place" : "\(places) places"
    }
}

extension LocationRequests {
    static func visit(_ ep: ChatEndpoint, _ body: PhoneVisitBody) -> URLRequest {
        request(ep, ["api", "link", "visit"], method: "POST", body: body.json())
    }

    static func places(_ ep: ChatEndpoint) -> URLRequest {
        request(ep, ["api", "link", "places"], method: "GET", body: nil)
    }

    static func forgetPlaces(_ ep: ChatEndpoint) -> URLRequest {
        request(ep, ["api", "link", "places", "forget"], method: "POST", body: Data("{}".utf8))
    }

    private static func request(_ ep: ChatEndpoint, _ path: [String], method: String, body: Data?) -> URLRequest {
        var url = ep.base
        for part in path { url = url.appending(component: part) }
        var r = URLRequest(url: url)
        r.httpMethod = method
        r.setValue("Bearer \(ep.token)", forHTTPHeaderField: "Authorization")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            r.setValue("application/json", forHTTPHeaderField: "Content-Type")
            r.httpBody = body
        }
        r.timeoutInterval = 20
        r.cachePolicy = .reloadIgnoringLocalCacheData
        return r
    }
}
