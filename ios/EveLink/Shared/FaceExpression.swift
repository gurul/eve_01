// eve's expressions in the voice-mode face (docs/voice-mode.md, Expressions). Owner, 2026-10-12: "can u route the
// expressions into the eyes". The daemon's picker (bridge live_expressions.py) reads the call's words and picks one
// label; the call's socket brings it down as
//   {"type": "expression", "id": <uint32, newer each time>, "label": "<label>", "ttl_ms": <ms>, "said": "<sentence>"}
// or {"type": "expression", "id": <uint32>, "clear": true} when the switch goes off. "said" is there when the pick is
// for one of eve's sentences: the face takes it when that sentence starts to play (VoiceLevels, on the playback clock
// its captions use), not when the message arrives. The face draws the shape the StackChan board drew for the same
// label (firmware/claude_pet_stackchan/src/expression.h), for the request's time to live counted from that start, and
// never while the owner is talking, muted or connecting.
//
// Pure and Foundation-only, so Tests/run.sh checks it on the Mac.
import Foundation

/// What the face is doing, from the call's state (VoiceModeView's LiveFace.mood).
enum FaceMood: Sendable, Equatable, CaseIterable {
    case idle, listening, thinking, speaking, muted, connecting
}

/// The picker's labels, in its order (bridge eye_model.LABELS). The names are the firmware's names.
enum FaceExpressionLabel: String, CaseIterable, Sendable {
    case calm, happy, curious, affection, surprised, sad, worried, skeptical, frustrated, excited, wink

    /// What VoiceOver says after the face's mood ("speaking, happy").
    var spoken: String {
        switch self {
        case .affection: "fond"
        case .wink: "winking"
        default: rawValue
        }
    }

    var style: FaceExpressionStyle {
        // expression.h, style(): {left, right, width, radius, mood, blink, curious}. Calm is the default row.
        switch self {
        case .wink:       FaceExpressionStyle(left: 96, right: 96, width: 96, radius: 22, lid: .none, blink: 4, curious: false)
        case .happy:      FaceExpressionStyle(left: 86, right: 86, width: 96, radius: 22, lid: .smile, blink: 3, curious: false)
        case .curious:    FaceExpressionStyle(left: 100, right: 88, width: 96, radius: 22, lid: .none, blink: 3, curious: true)
        case .affection:  FaceExpressionStyle(left: 76, right: 76, width: 100, radius: 32, lid: .smile, blink: 5, curious: true)
        case .surprised:  FaceExpressionStyle(left: 110, right: 110, width: 84, radius: 36, lid: .none, blink: 5, curious: false)
        case .sad:        FaceExpressionStyle(left: 72, right: 72, width: 92, radius: 22, lid: .droop, blink: 5, curious: false)
        case .worried:    FaceExpressionStyle(left: 102, right: 94, width: 90, radius: 26, lid: .droop, blink: 2, curious: true)
        case .skeptical:  FaceExpressionStyle(left: 58, right: 96, width: 94, radius: 18, lid: .none, blink: 4, curious: true)
        case .frustrated: FaceExpressionStyle(left: 70, right: 70, width: 94, radius: 16, lid: .brow, blink: 2, curious: false)
        case .excited:    FaceExpressionStyle(left: 110, right: 110, width: 104, radius: 26, lid: .smile, blink: 1, curious: false)
        case .calm:       FaceExpressionStyle.calm
        }
    }
}

/// The lid a shape draws over the eyes (RoboEyes' moods). The raw value is the firmware's `mood` number.
enum FaceLid: Int, Sendable, CaseIterable {
    case none = 0
    case smile = 1      // HAPPY: the bottom of each eye cut away by a rounded lid pushed up, so the eyes curve up
    case droop = 2      // TIRED: the outer top corner of each eye pulled down
    case brow = 3       // ANGRY: the inner top corner of each eye pulled down
}

/// One label's eye shape in the board's pixels (expression.h `Style`). The board's eye at rest is 96 x 96 with a
/// 22 px corner; the phone's rest eye (StackChanFace.eye) stands for that, so each number becomes a ratio of it.
struct FaceExpressionStyle: Equatable, Sendable {
    var left: Int           // the viewer's left eye's height, px (RoboEyes eyeL)
    var right: Int          // the viewer's right eye's height, px
    var width: Int
    var radius: Int
    var lid: FaceLid
    var blink: Int          // seconds between blinks (RoboEyes autoblinker, plus up to 1 s)
    var curious: Bool       // the outer eye grows when the eyes look to its side

    static let calm = FaceExpressionStyle(left: 96, right: 96, width: 96, radius: 22, lid: .none, blink: 3, curious: false)
    static let boardEye = 96.0
    static let boardRadius = 22.0
    /// RoboEyes grows the outer eye by 8 px when a curious face looks to that side.
    static let curiousGrow = 8.0 / boardEye

    var leftOpen: Double { Double(left) / Self.boardEye }
    var rightOpen: Double { Double(right) / Self.boardEye }
    var wide: Double { Double(width) / Self.boardEye }
    var corner: Double { Double(radius) / Self.boardRadius }
    var blinkEvery: ClosedRange<Double> { Double(blink)...Double(blink + 1) }
}

/// One request off the wire. `label` is nil for a clear.
struct FaceExpressionRequest: Equatable, Sendable {
    var id: UInt32
    var label: FaceExpressionLabel?
    /// Seconds the shape stays, from when it starts: the request's ttl_ms clamped to 0.5...6 s (expression.h).
    var ttl: Double
    /// The sentence of eve's the pick was made for (trimmed), or nil for the owner's words and a clear.
    var said: String?

    static let ttlRange: ClosedRange<Double> = 0.5...6.0
    static let defaultTTLms = 4000.0

    init(id: UInt32, label: FaceExpressionLabel?, ttlMs: Double = defaultTTLms, said: String? = nil) {
        self.id = id
        self.label = label
        ttl = min(max(ttlMs / 1000, Self.ttlRange.lowerBound), Self.ttlRange.upperBound)
        let words = said?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.said = label == nil || words.isEmpty ? nil : words
    }

    /// The firmware's parse (data.h): a nonzero uint32 id; `clear: true`, or a known label; ttl_ms a uint32 or
    /// 4000 ms. Anything else is nil and does nothing.
    static func decode(_ obj: [String: Any]) -> FaceExpressionRequest? {
        guard let id = whole(obj["id"]), id != 0 else { return nil }
        let clear = flag(obj["clear"])
        let label = (obj["label"] as? String).flatMap(FaceExpressionLabel.init(rawValue:))
        guard clear || label != nil else { return nil }
        return FaceExpressionRequest(id: UInt32(id), label: clear ? nil : label,
                                     ttlMs: whole(obj["ttl_ms"]).map { Double($0) } ?? defaultTTLms,
                                     said: obj["said"] as? String)
    }

    /// A JSON number that is a whole uint32 (not a boolean, which JSONSerialization also hands over as NSNumber).
    private static func whole(_ value: Any?) -> UInt64? {
        guard let n = value as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
        let d = n.doubleValue
        guard d.isFinite, d >= 0, d <= Double(UInt32.max), d == d.rounded(.towardZero) else { return nil }
        return UInt64(d)
    }

    private static func flag(_ value: Any?) -> Bool {
        guard let n = value as? NSNumber, CFGetTypeID(n) == CFBooleanGetTypeID() else { return false }
        return n.boolValue
    }
}

/// The expression the face may show now: which label, and which request it came from (a wink plays once per id).
struct FaceExpressionShown: Equatable, Sendable {
    var id: UInt32
    var label: FaceExpressionLabel
}

/// The requests taken, each from the time it starts, on a seconds clock (expression.h `State`). A request for one of
/// eve's sentences may start later than it arrived (when that sentence plays), so the one showing stays until then.
struct FaceExpressionState: Equatable, Sendable {
    /// The last request taken: the id check is against it, whenever it starts.
    private(set) var request: FaceExpressionRequest?
    /// What shows when: start times in order, ids rising. The newest entry whose start has come is the one showing.
    private(set) var timeline: [(at: Double, request: FaceExpressionRequest)] = []

    static func == (a: Self, b: Self) -> Bool {
        a.request == b.request && a.timeline.map(\.at) == b.timeline.map(\.at)
            && a.timeline.map(\.request) == b.timeline.map(\.request)
    }

    /// Takes a request only if its id is newer than the last one taken (int32 wrap-around, as the board compares),
    /// so a late or repeated message never brings an old shape back. A clear is a request too, and drops what was
    /// waiting. `start` (at least `now`) is when it shows; its time to live runs from then.
    @discardableResult
    mutating func accept(_ next: FaceExpressionRequest, now: Double, start: Double? = nil) -> Bool {
        guard next.id != 0 else { return false }
        if let last = request, !Self.newer(next.id, than: last.id) { return false }
        let at = next.label == nil ? now : max(now, start ?? now)
        request = next
        timeline.removeAll { $0.at > at }            // a newer request outranks what was waiting to start after it
        if let current = timeline.lastIndex(where: { $0.at <= now }) { timeline.removeFirst(current) }
        timeline.append((at, next))
        return true
    }

    /// Drops the requests that have not started yet (eve was interrupted: their sentences will not play).
    mutating func dropWaiting(now: Double) {
        timeline.removeAll { $0.at > now }
    }

    /// The shape to show at `now`, while the request's time to live runs.
    func shown(now: Double) -> FaceExpressionShown? {
        guard let entry = timeline.last(where: { $0.at <= now }), let label = entry.request.label,
              now - entry.at < entry.request.ttl else { return nil }
        return FaceExpressionShown(id: entry.request.id, label: label)
    }

    /// True while a request is still waiting to start after `now`.
    func waiting(now: Double) -> Bool {
        timeline.contains { $0.at > now }
    }

    static func newer(_ id: UInt32, than last: UInt32) -> Bool {
        Int32(bitPattern: id &- last) > 0
    }

    /// The board shows an expression only when it is not listening (eyes.cpp, expression::allowed). The phone's
    /// equivalent: eve resting, thinking or speaking. Never while the owner talks, the microphone is muted, or the call
    /// is still connecting.
    static func allowed(_ mood: FaceMood) -> Bool {
        switch mood {
        case .idle, .thinking, .speaking: true
        case .listening, .muted, .connecting: false
        }
    }

    static func gate(_ shown: FaceExpressionShown?, mood: FaceMood) -> FaceExpressionShown? {
        allowed(mood) ? shown : nil
    }
}

/// The wink (eyes.cpp, eyesTick): once per request id, the viewer's right eye closes for 650 ms into a curved lid and
/// a lifted brow shows over the open eye. It ends early when the wink stops showing (the owner talks, a new label).
struct FaceWink: Equatable, Sendable {
    static let hold = 0.65
    private var winked: UInt32?
    private(set) var since: Double?

    /// True while the wink holds at `now`.
    mutating func step(_ shown: FaceExpressionShown?, now: Double) -> Bool {
        if let shown, shown.label == .wink, shown.id != winked {
            winked = shown.id
            since = now
        }
        guard let start = since else { return false }
        if shown?.label == .wink, now - start < Self.hold { return true }
        since = nil
        return false
    }

    /// The closed lid's rise above the eye's centre line, in board pixels, `x` along a lid `width` wide: a flat middle
    /// raised 8 px with rounded 22 px corner zones (expression.h winkCurveY), so it reads as an arch. Negative is up.
    static func curve(_ x: Double, width: Double, corner: Double = 22, rise: Double = 8) -> Double {
        guard width > 0 else { return 0 }
        let radius = min(width / 2, corner)
        guard radius > 0 else { return 0 }
        let edge = min(x, width - x)
        if edge >= radius { return -rise }
        let t = (radius - max(edge, 0)) / radius
        return -rise * (1 - t * t).squareRoot()
    }
}
