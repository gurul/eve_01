// eve's controls in the Work view (docs/app-chat.md, "Work"): the Telegram Mini App's Music, Lights and TV remote
// cards, drawn natively, and any other app the Mac's registry has (app_registry.py), drawn by the generic sheet
// (GenericAppSheet.swift). The daemon's link door hands each body to the app's own panel (link_chat.py), so the
// actions and answers are exactly the Mini App's (miniapp_page.html). Foundation only, so Tests/main.swift checks it
// on the Mac.
//
//   GET  /api/link/controls             {"lights": [{"name", "room"?}], "spotify": bool, "firetv": bool, "memory": bool,
//                                        "notes": bool, "pika": bool, "obsbot": bool,
//                                        "apps": [{"id", "title", "symbol", "detail", "sheet": "native"|"generic"}]}
//                                       ("apps": the present apps in the Mac's order; an older Mac has no "apps",
//                                        and the legacy keys give the rows)
//   POST /api/link/controls/lights      {"action": "status"} | {"action": "set", "target", "power"|"color"|"brightness"}
//   POST /api/link/controls/spotify     {"action": "status"|"play"|"pause"|"next"|"previous"|"volume"|"transfer"|"mode"}
//   POST /api/link/controls/firetv      {"action": "status"|"up"|...|"volume"|"volume_change"|"open_app"|"power_on"...}
//   POST /api/link/controls/memory      {"action": "profile"|"search"|"forget"|"apply"|"remember"|"hide"|"unhide"|"hidden",
//                                        "query"?, "token"?, "text"?}
//                                       (link_memory.py)
//   POST /api/link/controls/pika        hold-to-drive for the car, and its video (PikaDrive.swift, pika.py)
//   POST /api/link/controls/obsbot      the OBSBOT webcam, and its picture (ObsbotControl.swift, obsbot_panel.py)
//   POST /api/link/controls/<id>        any other app: {"action": "status"} first, then {"action"}, {"action", "on"}
//                                        or {"action", "value"}; the answer is an AppPanel (docs/adding-an-app.md)
//   404 with {"ok": false, "line"} for a control this Mac does not have.
import Foundation

/// One of eve's controls, in the order the Work view lists them.
enum ControlKind: String, CaseIterable, Sendable, Identifiable {
    case notes, spotify, lights, firetv, pika, obsbot, memory

    var id: String { rawValue }

    var title: String {
        switch self {
        case .spotify: "Music"
        case .lights: "Lights"
        case .firetv: "Remote"
        case .memory: "Memory"
        case .notes: "Notes"
        case .pika: "Pika"
        case .obsbot: "Camera"
        }
    }

    /// The row's SF Symbol.
    var symbol: String {
        switch self {
        case .spotify: "music.note"
        case .lights: "lightbulb"
        case .firetv: "tv"
        case .memory: "brain.head.profile"
        case .notes: "doc.text"
        case .pika: "car"
        case .obsbot: "web.camera"
        }
    }
}

/// A light's name and room, as the owner's lights file has them (never an address).
struct LightName: Equatable, Sendable, Hashable {
    var name: String
    var room: String?
}

/// Which controls this Mac has.
struct ControlsInfo: Equatable, Sendable {
    var lights: [LightName]
    var spotify: Bool
    var firetv: Bool
    var memory: Bool = false        // the Memory card (2026-10-09): on while the Mac's memory is on
    var notes: Bool = false         // Notes (2026-10-10, link_notes.py): on while the second brain's vault is there
    var pika: Bool = false          // Pika, the car (2026-10-10, pika.py): only with EVE_PIKA=1 and its config
    var obsbot: Bool = false        // the OBSBOT webcam (2026-10-11, obsbot_panel.py): while OBSBOT Center is installed
    /// The Mac's own list of its apps (app_registry.py, 2026-10-12), in its order; nil from a Mac older than that.
    var entries: [AppEntry]? = nil

    var available: [ControlKind] {
        ControlKind.allCases.filter { kind in
            switch kind {
            case .spotify: spotify
            case .lights: !lights.isEmpty
            case .firetv: firetv
            case .memory: memory
            case .notes: notes
            case .pika: pika
            case .obsbot: obsbot
            }
        }
    }

    /// The Apps tab's rows: what this Mac has, except Memory, which is in Settings (owner, 2026-10-10: "move memories
    /// to settings").
    var apps: [ControlKind] { available.filter { $0 != .memory } }

    /// The Apps tab's rows. Notes first when the Mac has it (it is eve's own, not a registry app), then the Mac's
    /// apps in the Mac's order: the native sheet for an app the phone has Swift for, the generic sheet for any other.
    /// A Mac with no "apps" list gives today's rows from the legacy keys.
    var rows: [AppRow] {
        guard let entries else { return apps.map(AppRow.native) }
        var out: [AppRow] = notes ? [.native(.notes)] : []
        var seen: Set<String> = [ControlKind.notes.rawValue, ControlKind.memory.rawValue]   // never a registry row
        for e in entries where seen.insert(e.id).inserted {
            if e.sheet == AppEntry.nativeSheet, let kind = ControlKind(rawValue: e.id) {
                out.append(.native(kind))
            } else {
                out.append(.generic(e))
            }
        }
        return out
    }

    /// The Mac's words for a native app's row, when it sent them.
    func entry(for kind: ControlKind) -> AppEntry? { entries?.first { $0.id == kind.rawValue } }

    static func decode(_ data: Data) -> ControlsInfo? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let lights = (obj["lights"] as? [[String: Any]] ?? []).compactMap { l -> LightName? in
            guard let name = l["name"] as? String, !name.isEmpty else { return nil }
            return LightName(name: name, room: (l["room"] as? String).flatMap { $0.isEmpty ? nil : $0 })
        }
        return ControlsInfo(lights: lights, spotify: obj["spotify"] as? Bool ?? false,
                            firetv: obj["firetv"] as? Bool ?? false, memory: obj["memory"] as? Bool ?? false,
                            notes: obj["notes"] as? Bool ?? false, pika: obj["pika"] as? Bool ?? false,
                            obsbot: obj["obsbot"] as? Bool ?? false,
                            entries: (obj["apps"] as? [Any]).map { $0.compactMap(AppEntry.init(json:)) })
    }
}

/// One app as the Mac lists it under "apps" (app_registry.for_phone): its slot, the row's words and SF Symbol, and
/// whether the phone has a native sheet for it.
struct AppEntry: Equatable, Sendable, Hashable, Identifiable {
    var id: String              // the slot: POST /api/link/controls/<id>
    var title: String
    var symbol: String          // an SF Symbol name; the row falls back when this iOS does not have it
    var detail: String
    var sheet: String           // "native" | "generic"; anything else is generic

    static let nativeSheet = "native"
    static let fallbackSymbol = "square.grid.2x2"
    static let maxTitle = 40
    static let maxDetail = 80

    /// The registry's slot rule (app_template: ^[a-z][a-z0-9]{1,15}$; link_chat's CONTROL_RE allows one letter).
    static func validID(_ id: String) -> Bool {
        guard let first = id.unicodeScalars.first, id.unicodeScalars.count <= 16, ("a"..."z").contains(first) else {
            return false
        }
        return id.unicodeScalars.allSatisfy { ("a"..."z").contains($0) || ("0"..."9").contains($0) }
    }

    init(id: String, title: String, symbol: String = AppEntry.fallbackSymbol, detail: String = "",
         sheet: String = "generic") {
        self.id = id
        self.title = title
        self.symbol = symbol
        self.detail = detail
        self.sheet = sheet
    }

    /// An entry with a valid id; the other fields are optional and trimmed.
    init?(json: Any) {
        guard let o = json as? [String: Any], let id = o["id"] as? String, Self.validID(id) else { return nil }
        let title = AppText.trimmed(o["title"] as? String, Self.maxTitle)
        let symbol = AppText.trimmed(o["symbol"] as? String, 64)
        self.init(id: id, title: title.isEmpty ? id.prefix(1).uppercased() + id.dropFirst() : title,
                  symbol: symbol.isEmpty ? Self.fallbackSymbol : symbol,
                  detail: AppText.trimmed(o["detail"] as? String, Self.maxDetail),
                  sheet: o["sheet"] as? String ?? "generic")
    }
}

/// One row of the Apps tab: an app with a native sheet, or one the generic sheet draws.
enum AppRow: Equatable, Sendable, Hashable, Identifiable {
    case native(ControlKind)
    case generic(AppEntry)

    var id: String {
        switch self {
        case .native(let kind): kind.rawValue
        case .generic(let entry): entry.id
        }
    }
}

enum AppText {
    /// Whitespace-trimmed and cut to `limit` characters (with an ellipsis when cut); "" for nil.
    static func trimmed(_ s: String?, _ limit: Int) -> String {
        let t = (s ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        return t.count <= limit ? t : String(t.prefix(max(0, limit - 1))) + "…"
    }
}

// ---- memory -------------------------------------------------------------------------------------------------------

/// One fact from the owner's records: its record id and the line, with where it came from when known.
struct MemoryFact: Equatable, Sendable, Hashable {
    var id: String
    var line: String
    var src: String?
}

/// One line that was said: the day, the time, the channel, who, and the words.
struct MemorySaid: Equatable, Sendable, Hashable {
    var day: String
    var time: String
    var channel: String
    var who: String
    var text: String
}

/// The Memory card's answer: the profile, a search's three groups, a forget preview's counts and token, or a
/// forget's result (link_memory.py).
struct MemoryReply: Equatable, Sendable {
    var ok: Bool
    var line: String
    var profile: String?
    var hits: [MemoryFact] = []
    var recalled: [String] = []
    var said: [MemorySaid] = []
    var total: Int?
    var token: String?
    var hidden: [String] = []       // "Don't mention this again": what is left out of the profile and searches

    var isEmpty: Bool { hits.isEmpty && recalled.isEmpty && said.isEmpty }

    static func decode(_ data: Data) -> MemoryReply? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let hits = (obj["hits"] as? [[String: Any]] ?? []).compactMap { h -> MemoryFact? in
            guard let line = h["line"] as? String, !line.isEmpty else { return nil }
            return MemoryFact(id: h["id"] as? String ?? "", line: line,
                              src: (h["src"] as? String).flatMap { $0.isEmpty ? nil : $0 })
        }
        let said = (obj["said"] as? [[String: Any]] ?? []).compactMap { s -> MemorySaid? in
            guard let text = s["text"] as? String, !text.isEmpty else { return nil }
            return MemorySaid(day: s["day"] as? String ?? "", time: s["time"] as? String ?? "",
                              channel: s["ch"] as? String ?? "", who: s["who"] as? String ?? "", text: text)
        }
        return MemoryReply(ok: obj["ok"] as? Bool ?? false, line: obj["line"] as? String ?? "",
                           profile: obj["profile"] as? String, hits: hits,
                           recalled: (obj["recalled"] as? [String] ?? []).filter { !$0.isEmpty }, said: said,
                           total: obj["total"] as? Int, token: (obj["token"] as? String).flatMap { $0.isEmpty ? nil : $0 },
                           hidden: (obj["hidden"] as? [String] ?? []).filter { !$0.isEmpty })
    }
}

// ---- lights -------------------------------------------------------------------------------------------------------

/// One choice of which lights a change goes to: every light, a room, or one light (the Mini App's chips).
struct LightChoice: Equatable, Sendable, Hashable, Identifiable {
    var value: String           // the panel's "target"
    var label: String
    var id: String { value }

    /// "All", then each room, then each light; rooms and lights only when there is more than one light.
    static func choices(_ lights: [LightName]) -> [LightChoice] {
        var out = [LightChoice(value: "all", label: "All")]
        guard lights.count > 1 else { return out }
        var seen = Set<String>()
        for room in lights.compactMap(\.room) where seen.insert(room).inserted {
            out.append(LightChoice(value: room, label: capitalized(room)))
        }
        for l in lights { out.append(LightChoice(value: l.name, label: capitalized(l.name))) }
        return out
    }

    private static func capitalized(_ s: String) -> String { s.prefix(1).uppercased() + s.dropFirst() }
}

/// The colours the Mini App offers, by the names lights.parse_color knows.
enum LightSwatch {
    static let all: [(name: String, hex: String)] = [
        ("red", "#ff2a2a"), ("orange", "#ff7a1a"), ("yellow", "#ffd21a"), ("green", "#1ed760"), ("cyan", "#1ae0ff"),
        ("blue", "#2a5cff"), ("purple", "#8c2aff"), ("pink", "#ff4fa3"), ("warm white", "#ffd9a0"), ("white", "#fff6ea"),
    ]

    /// "#rrggbb" from 0...1 components, clamped.
    static func hex(red: Double, green: Double, blue: Double) -> String {
        func byte(_ v: Double) -> Int { Int((max(0, min(1, v)) * 255).rounded()) }
        return String(format: "#%02x%02x%02x", byte(red), byte(green), byte(blue))
    }

    /// 0...1 components of "#rrggbb", or nil.
    static func rgb(_ hex: String) -> (Double, Double, Double)? {
        let h = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
        guard h.count == 6, let v = UInt32(h, radix: 16) else { return nil }
        return (Double((v >> 16) & 0xff) / 255, Double((v >> 8) & 0xff) / 255, Double(v & 0xff) / 255)
    }
}

/// One light as a status read has it.
struct LightState: Equatable, Sendable {
    var name: String
    var reachable: Bool
    var on: Bool
    var brightness: Int?
    var color: String?          // a colour word ("cyan")
    var rgb: String?            // "#19ffff"
    var whiteKelvin: Int?

    /// "off", "cyan 60%", "not answering".
    var said: String {
        if !reachable { return "not answering" }
        if !on { return "off" }
        let words = [color ?? (whiteKelvin != nil ? "white" : nil), brightness.map { "\($0)%" }].compactMap { $0 }
        return words.isEmpty ? "on" : words.joined(separator: " ")
    }
}

/// The lights panel's answer: a status read's lights, or a change's line.
struct LightsReply: Equatable, Sendable {
    var ok: Bool
    var line: String
    var lights: [LightState]

    /// What the sheet's line says: the change's line, or each light's state after a status read.
    var said: String {
        if !line.isEmpty { return line }
        return lights.map { "\($0.name): \($0.said)" }.joined(separator: " · ")
    }

    /// The brightness the slider starts at: the first light that answered.
    var brightness: Int? { lights.first(where: \.reachable)?.brightness }

    static func decode(_ data: Data) -> LightsReply? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let lights = (obj["lights"] as? [[String: Any]] ?? []).compactMap { l -> LightState? in
            guard let name = l["name"] as? String else { return nil }
            return LightState(name: name, reachable: l["reachable"] as? Bool ?? false, on: l["on"] as? Bool ?? false,
                              brightness: (l["brightness"] as? NSNumber)?.intValue, color: l["color"] as? String,
                              rgb: l["rgb"] as? String, whiteKelvin: (l["white_kelvin"] as? NSNumber)?.intValue)
        }
        let line = (obj["line"] as? String) ?? (obj["reason"] as? String) ?? ""
        return LightsReply(ok: obj["ok"] as? Bool ?? false, line: line, lights: lights)
    }
}

// ---- music --------------------------------------------------------------------------------------------------------

struct MusicDevice: Equatable, Sendable, Identifiable, Hashable {
    var name: String
    var active: Bool
    var restricted: Bool
    var id: String { name }
}

/// The Spotify panel's answer.
struct MusicState: Equatable, Sendable {
    var ok: Bool
    var line: String
    var playing: Bool
    var track: String?
    var artists: [String]
    var album: String
    var art: URL?               // only Spotify's own image host
    var volume: Int?            // the device playing now
    var devices: [MusicDevice]
    var mode: Bool?             // the Voice PE's Spotify mode, when the board is eve's controller

    var byLine: String { [artists.joined(separator: ", "), album].filter { !$0.isEmpty }.joined(separator: " · ") }

    static let artHost = "i.scdn.co"

    static func decode(_ data: Data) -> MusicState? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let device = obj["device"] as? [String: Any]
        let art = (obj["art"] as? String).flatMap(URL.init(string:)).flatMap { u in
            u.scheme == "https" && u.host == artHost ? u : nil
        }
        let devices = (obj["devices"] as? [[String: Any]] ?? []).compactMap { d -> MusicDevice? in
            guard let name = d["name"] as? String, !name.isEmpty else { return nil }
            return MusicDevice(name: name, active: d["active"] as? Bool ?? false,
                               restricted: d["restricted"] as? Bool ?? false)
        }
        let line = (obj["line"] as? String) ?? (obj["reason"] as? String) ?? ""
        return MusicState(ok: obj["ok"] as? Bool ?? false, line: line, playing: obj["playing"] as? Bool ?? false,
                          track: (obj["track"] as? String).flatMap { $0.isEmpty ? nil : $0 },
                          artists: obj["artists"] as? [String] ?? [], album: obj["album"] as? String ?? "",
                          art: art, volume: (device?["volume"] as? NSNumber)?.intValue, devices: devices,
                          mode: obj["mode"] as? Bool)
    }
}

// ---- the TV remote ------------------------------------------------------------------------------------------------

/// The Fire TV panel's answer.
struct TvState: Equatable, Sendable {
    var ok: Bool
    var line: String
    var now: String?            // "TV on — YouTube, volume 44%"
    var power: String?          // on | screensaver | standby | unknown
    var app: String?
    var volume: Int?
    var muted: Bool?
    var apps: [String]

    var isOn: Bool { power == "on" || power == "screensaver" }

    static func decode(_ data: Data) -> TvState? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let line = (obj["line"] as? String) ?? (obj["reason"] as? String) ?? ""
        return TvState(ok: obj["ok"] as? Bool ?? false, line: line, now: obj["now"] as? String,
                       power: obj["power"] as? String, app: obj["app"] as? String,
                       volume: (obj["volume"] as? NSNumber)?.intValue, muted: obj["muted"] as? Bool,
                       apps: obj["apps"] as? [String] ?? [])
    }
}

/// One press on the remote, as the panel takes it.
struct TvPress: Equatable, Sendable {
    var action: String
    var count = 1               // d-pad taps made while one was on its way: one press with a repeat
    var volume: Int?
    var change: Int?
    var app: String?

    static let navKeys: Set<String> = ["up", "down", "left", "right", "select", "back", "home", "menu"]
    var isNav: Bool { Self.navKeys.contains(action) }

    var body: [String: Any] {
        var b: [String: Any] = ["action": action]
        if count > 1 { b["repeat"] = count }
        if let volume { b["volume"] = volume }
        if let change { b["change"] = change }
        if let app { b["app"] = app }
        return b
    }
}

/// The remote's presses in the order they were made (the TV gets them in that order, one at a time). Taps on the
/// same d-pad key while one is on its way go as one press with a count; a newer slider volume replaces an unsent one.
struct TvQueue: Equatable, Sendable {
    private(set) var pending: [TvPress] = []     // pending[0] is on its way while `sending`
    private(set) var sending = false

    mutating func push(_ press: TvPress) {
        let waiting = pending.count > (sending ? 1 : 0)
        if waiting, var last = pending.last {
            if last.action == press.action && press.isNav {
                last.count += 1
                pending[pending.count - 1] = last
                return
            }
            if last.action == "volume" && press.action == "volume" {
                last.volume = press.volume
                pending[pending.count - 1] = last
                return
            }
        }
        pending.append(press)
    }

    /// The next press to send, marking it on its way; nil while one is on its way or nothing waits.
    mutating func next() -> TvPress? {
        guard !sending, let first = pending.first else { return nil }
        sending = true
        return first
    }

    /// The press on its way was answered.
    mutating func finished() {
        if sending, !pending.isEmpty { pending.removeFirst() }
        sending = false
    }
}

// ---- any other app: the generic sheet (GenericAppSheet.swift, docs/adding-an-app.md) ---------------------------------

/// One control the app's panel offers. Unknown types never get here: the decoder skips them, so the format can grow.
struct AppControl: Equatable, Sendable, Hashable, Identifiable {
    enum Kind: Equatable, Sendable, Hashable {
        case button(symbol: String?, destructive: Bool)
        case toggle(on: Bool)
        case slider(value: Double, min: Double, max: Double, step: Double?)
    }

    var id: String
    var label: String
    var action: String
    var kind: Kind

    static let maxLabel = 40

    /// A control from the panel's JSON, or nil: an unknown type, no action, or a slider whose range is empty.
    init?(json: Any) {
        guard let o = json as? [String: Any], let type = o["type"] as? String,
              let action = o["action"] as? String, !action.isEmpty, action.count <= 64 else { return nil }
        let id = (o["id"] as? String).flatMap { $0.isEmpty || $0.count > 64 ? nil : $0 } ?? action
        let label = AppText.trimmed(o["label"] as? String, Self.maxLabel)
        switch type {
        case "button":
            let symbol = AppText.trimmed(o["symbol"] as? String, 64)
            kind = .button(symbol: symbol.isEmpty ? nil : symbol, destructive: o["role"] as? String == "destructive")
        case "toggle":
            kind = .toggle(on: o["on"] as? Bool ?? false)
        case "slider":
            guard let lo = Self.number(o["min"]), let hi = Self.number(o["max"]), lo < hi else { return nil }
            let step = Self.number(o["step"]).flatMap { $0 > 0 && $0 <= hi - lo ? $0 : nil }
            let value = Self.snap(Self.number(o["value"]) ?? lo, min: lo, max: hi, step: step)
            kind = .slider(value: value, min: lo, max: hi, step: step)
        default:
            return nil
        }
        self.id = id
        self.label = label.isEmpty ? action : label
        self.action = action
    }

    init(id: String, label: String, action: String, kind: Kind) {
        self.id = id
        self.label = label
        self.action = action
        self.kind = kind
    }

    /// What a press sends: {"action"} for a button, {"action", "on"} for a toggle, {"action", "value"} for a slider.
    /// `on` and `value` are the new setting; without one the control's current setting goes.
    func body(on: Bool? = nil, value: Double? = nil) -> [String: Any] {
        var b: [String: Any] = ["action": action]
        switch kind {
        case .button: break
        case .toggle(let now): b["on"] = on ?? now
        case .slider(let now, let lo, let hi, let step):
            let v = Self.snap(value ?? now, min: lo, max: hi, step: step)
            b["value"] = v == v.rounded() && abs(v) < 1e15 ? Int(v) as Any : v as Any
        }
        return b
    }

    /// The same control showing a new setting (what the phone shows while the Mac answers).
    func showing(on: Bool? = nil, value: Double? = nil) -> AppControl {
        var c = self
        switch kind {
        case .button: break
        case .toggle(let now): c.kind = .toggle(on: on ?? now)
        case .slider(let now, let lo, let hi, let step):
            c.kind = .slider(value: Self.snap(value ?? now, min: lo, max: hi, step: step), min: lo, max: hi, step: step)
        }
        return c
    }

    /// `value` held to min...max, on a step from min when there is one.
    static func snap(_ value: Double, min lo: Double, max hi: Double, step: Double?) -> Double {
        guard value.isFinite else { return lo }
        var v = Swift.min(hi, Swift.max(lo, value))
        if let step, step > 0 {
            v = lo + ((v - lo) / step).rounded() * step
            v = Swift.min(hi, Swift.max(lo, v))
        }
        return v
    }

    /// A finite JSON number; true and false are not numbers here.
    static func number(_ any: Any?) -> Double? {
        guard let n = any as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
        let d = n.doubleValue
        return d.isFinite ? d : nil
    }
}

/// The app's answer to a press (app_registry.panel): the headline status, the muted line, and the controls. nil
/// controls means the answer has none, and the sheet keeps the ones it shows.
struct AppPanel: Equatable, Sendable {
    var ok: Bool
    var ui: Int
    var line: String
    var status: String?
    var controls: [AppControl]?

    static let maxControls = 24
    static let maxLine = 300
    static let maxStatus = 120

    static func decode(_ data: Data) -> AppPanel? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        var controls: [AppControl]?
        if let raw = obj["controls"] as? [Any] {
            var seen = Set<String>()
            controls = Array(raw.compactMap(AppControl.init(json:)).filter { seen.insert($0.id).inserted }
                .prefix(maxControls))
        }
        let line = AppText.trimmed((obj["line"] as? String) ?? (obj["reason"] as? String), maxLine)
        let status = AppText.trimmed(obj["status"] as? String, maxStatus)
        return AppPanel(ok: obj["ok"] as? Bool ?? false, ui: (obj["ui"] as? NSNumber)?.intValue ?? 1, line: line,
                        status: status.isEmpty ? nil : status, controls: controls)
    }
}

/// What the generic sheet shows, as plain values: each answer redraws it, an answer without controls keeps the last
/// ones, a press shows its new setting at once and takes it back if the press fails. One press at a time.
struct AppPanelModel: Equatable, Sendable {
    private(set) var status: String?
    private(set) var line = ""
    private(set) var failed = false
    private(set) var controls: [AppControl] = []
    private(set) var busy = false
    private(set) var answered = false            // the first answer came (until then the sheet shows a spinner)
    private var before: [AppControl]?             // what to show again if the press on its way fails

    static let noAnswer = "eve didn't answer. Is the Mac awake?"

    /// The body for a press on `control`, showing its new setting; nil while another press is on its way.
    mutating func press(_ control: AppControl, on: Bool? = nil, value: Double? = nil) -> [String: Any]? {
        guard !busy, let i = controls.firstIndex(where: { $0.id == control.id }) else { return nil }
        busy = true
        before = controls
        controls[i] = controls[i].showing(on: on, value: value)
        return controls[i].body(on: on, value: value)
    }

    /// A status read is on its way (on open, or a pull to refresh); nil while another call is.
    mutating func refresh() -> [String: Any]? {
        guard !busy else { return nil }
        busy = true
        before = nil
        return ["action": "status"]
    }

    /// The Mac answered, or did not.
    mutating func finish(_ result: Result<AppPanel, ChatFailure>) {
        busy = false
        answered = true
        switch result {
        case .success(let p):
            if let c = p.controls { controls = c } else if !p.ok, let before { controls = before }
            if let s = p.status { status = s }
            line = p.line.isEmpty && !p.ok ? "That didn't work." : p.line
            failed = !p.ok
        case .failure(let f):
            if let before { controls = before }
            line = f.message.isEmpty ? Self.noAnswer : f.message
            failed = true
        }
        before = nil
    }
}

// ---- the requests -------------------------------------------------------------------------------------------------

enum ControlsRequests {
    static func list(_ ep: ChatEndpoint) -> URLRequest {
        WorkRequests.request(ep, ["api", "link", "controls"], post: false)
    }

    static func press(_ ep: ChatEndpoint, _ kind: ControlKind, body: [String: Any]) -> URLRequest {
        pressApp(ep, id: kind.rawValue, body: body)
    }

    /// A press on any app by its slot: the same door as the native sheets' (POST /api/link/controls/<id>).
    static func pressApp(_ ep: ChatEndpoint, id: String, body: [String: Any]) -> URLRequest {
        var r = WorkRequests.request(ep, ["api", "link", "controls", id], post: true, body: body)
        r.timeoutInterval = 30          // a Bluetooth light or a sleeping TV takes a few seconds
        return r
    }
}

extension WorkClient {
    func controls() async -> Result<ControlsInfo, ChatFailure> {
        await call(.controls, ControlsRequests.list) { data, _ in ControlsInfo.decode(data) }
    }

    func lights(_ body: [String: Any]) async -> Result<LightsReply, ChatFailure> {
        await call(.controls, { ControlsRequests.press($0, .lights, body: body) }) { data, _ in LightsReply.decode(data) }
    }

    func music(_ body: [String: Any]) async -> Result<MusicState, ChatFailure> {
        await call(.controls, { ControlsRequests.press($0, .spotify, body: body) }) { data, _ in MusicState.decode(data) }
    }

    func tv(_ body: [String: Any]) async -> Result<TvState, ChatFailure> {
        await call(.controls, { ControlsRequests.press($0, .firetv, body: body) }) { data, _ in TvState.decode(data) }
    }

    func memory(_ body: [String: Any]) async -> Result<MemoryReply, ChatFailure> {
        await call(.controls, { ControlsRequests.press($0, .memory, body: body) }) { data, _ in MemoryReply.decode(data) }
    }

    /// A press on an app the generic sheet draws.
    func app(_ id: String, _ body: [String: Any]) async -> Result<AppPanel, ChatFailure> {
        await call(.controls, { ControlsRequests.pressApp($0, id: id, body: body) }) { data, _ in AppPanel.decode(data) }
    }

    func notes(_ body: [String: Any]) async -> Result<NotesReply, ChatFailure> {
        await call(.controls, { ControlsRequests.press($0, .notes, body: body) }) { data, _ in NotesReply.decode(data) }
    }
}
