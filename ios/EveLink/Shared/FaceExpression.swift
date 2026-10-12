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

/// The picker's labels, in its order (bridge eye_model.LABELS): the board's eleven, then the eight of the 2026-10-12
/// round (docs/expressions-design.json). The names are the firmware's names.
enum FaceExpressionLabel: String, CaseIterable, Sendable {
    case calm, happy, curious, affection, surprised, sad, worried, skeptical, frustrated, excited, wink
    case laughing, sleepy, confused, thinking, sheepish, relieved, proud, flattered

    /// What VoiceOver says after the face's mood ("speaking, happy").
    var spoken: String {
        switch self {
        case .affection: "fond"
        case .wink: "winking"
        case .thinking: "mulling it over"     // the mood is often "thinking" too: never "thinking, thinking"
        case .flattered: "flattered, blushing"
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
        // The 2026-10-12 round (docs/expressions-design.json, "shape"). Heights are the eye before its lid: laughing's
        // 94 under a smile lid at 1.4 (70% cover) leaves the design's 28 px crescent.
        case .laughing:   FaceExpressionStyle(left: 94, right: 94, width: 100, radius: 22, lid: .smile, blink: 3, curious: false,
                                              depth: 1.4, ttlMs: 3000, easeIn: 40)
        case .sleepy:     FaceExpressionStyle(left: 40, right: 40, width: 96, radius: 18, lid: .flat, blink: 6, curious: false,
                                              blinkShut: 0.4, ttlMs: 5000)
        case .confused:   FaceExpressionStyle(left: 100, right: 88, width: 92, radius: 22, lid: .none, blink: 3, curious: false,
                                              ttlMs: 3500)
        case .thinking:   FaceExpressionStyle(left: 84, right: 84, width: 88, radius: 24, lid: .flat, blink: 3, curious: false,
                                              ttlMs: 3500)
        case .sheepish:   FaceExpressionStyle(left: 78, right: 78, width: 92, radius: 22, lid: .flat, blink: 2, curious: false)
        case .relieved:   FaceExpressionStyle(left: 86, right: 86, width: 98, radius: 26, lid: .droopSmile, blink: 4,
                                              curious: false, ttlMs: 3500)
        case .proud:      FaceExpressionStyle(left: 80, right: 80, width: 100, radius: 24, lid: .smile, blink: 3, curious: false,
                                              depth: 0.5)
        case .flattered:  FaceExpressionStyle(left: 66, right: 66, width: 100, radius: 28, lid: .smile, blink: 4, curious: false,
                                              depth: 0.8)
        }
    }
}

/// The lid a shape draws over the eyes (RoboEyes' moods, and two more). The raw value is the firmware's `mood` number.
enum FaceLid: Int, Sendable, CaseIterable {
    case none = 0
    case smile = 1      // HAPPY: the bottom of each eye cut away by a rounded lid pushed up, so the eyes curve up
    case droop = 2      // TIRED: the outer top corner of each eye pulled down
    case brow = 3       // ANGRY: the inner top corner of each eye pulled down
    case flat = 4       // a level upper lid: the top corners squared off and the top lowered, the bottom kept in place
    case droopSmile = 5 // relieved: the outer droop at half of sad's depth, over a smile at 70% of happy's
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
    /// How far the lid comes in, as a share of the board's full mood (a smile at 1 cuts half the eye away).
    var depth: Double = 1
    /// How long one blink keeps the lids shut, seconds (sleepy's slow 400 ms close).
    var blinkShut: Double = 0.11
    /// The label's time to live when a request carries none (expression.h Request.ttl, per kind).
    var ttlMs: Double = 4000
    /// How fast the face eases into this shape (a spring's omega, 1/s). 12 is about a third of a second; laughing's
    /// 40 reaches its crescent in about 120 ms, so its first look is not an excited or happy one.
    var easeIn: Double = 12

    static let calm = FaceExpressionStyle(left: 96, right: 96, width: 96, radius: 22, lid: .none, blink: 3, curious: false)
    static let boardEye = 96.0
    static let boardRadius = 22.0
    /// RoboEyes grows the outer eye by 8 px when a curious face looks to that side.
    static let curiousGrow = 8.0 / boardEye
    static let reliefSmile = 0.7

    var leftOpen: Double { Double(left) / Self.boardEye }
    var rightOpen: Double { Double(right) / Self.boardEye }
    var wide: Double { Double(width) / Self.boardEye }
    var corner: Double { Double(radius) / Self.boardRadius }
    var blinkEvery: ClosedRange<Double> { Double(blink)...Double(blink + 1) }

    /// Each lid's depth, 0 (out) to 1 (the board's full mood) or more (laughing's 70% smile).
    /// relieved's smile is 70% of happy's (the contact sheet read a lighter one as sad), so the droop never wins.
    var smile: Double { lid == .smile ? depth : lid == .droopSmile ? Self.reliefSmile * depth : 0 }
    var droop: Double { lid == .droop ? depth : lid == .droopSmile ? 0.5 * depth : 0 }
    var brow: Double { lid == .brow ? depth : 0 }
    var flat: Double { lid == .flat ? depth : 0 }
}

/// A small white mark drawn beside the eyes (expressions-design.json "Overlay"), in the face's own white. Offsets and
/// sizes are board px from the anchor; `scale` grows it in and out (0 is not drawn), since the board draws no partial
/// opacity; `angle` tips it, degrees, positive toward the viewer's right.
struct FaceOverlay: Equatable, Sendable {
    enum Kind: String, Sendable, CaseIterable { case z, question, sweat, sparkle, blush }
    enum Anchor: Sendable, Equatable {
        case aboveRightEye      // over the outer half of the viewer's right eye
        case aboveCentre        // centred over the pair
        case outerTop(right: Bool)   // the outer top corner of one eye
        case under(right: Bool)      // centred under one eye
    }
    var kind: Kind
    var anchor: Anchor
    var dx = 0.0
    var dy = 0.0
    var size: Double
    var scale = 1.0
    var angle = 0.0
}

/// A label's pose at one moment: the shape's heights and gaze as the motion moves them, in board px. Pure in
/// (label, seconds since it showed), so Tests/Face checks it; StackChanFace's FaceMotion eases it and draws it.
struct FaceExpressionPose: Equatable, Sendable {
    var left: Double
    var right: Double
    /// Where the expression points the gaze (board px, + right and down), or nil: the mood keeps the gaze.
    var gaze: (x: Double, y: Double)?
    /// A quick move of both eyes on top (bounce, sway, shuffle, chin-up), board px.
    var jiggle: (x: Double, y: Double) = (0, 0)
    /// Both eyes shut into the wink's arched line, 0 open...1 shut (relieved's exhale).
    var closed = 0.0
    /// A scripted blink, 0 open...1 shut (proud's deliberate one).
    var shut = 0.0
    /// Whether the random blink may land now.
    var blinks = true
    var overlays: [FaceOverlay] = []

    static func == (a: Self, b: Self) -> Bool {
        a.left == b.left && a.right == b.right && a.gaze?.x == b.gaze?.x && a.gaze?.y == b.gaze?.y
            && a.jiggle == b.jiggle && a.closed == b.closed && a.shut == b.shut && a.blinks == b.blinks
            && a.overlays == b.overlays
    }
}

extension FaceExpressionLabel {
    /// From this time (seconds since shown) the pose is still, or repeats every `period`.
    var settle: Double {
        switch self {
        case .laughing: 1.2
        case .sleepy: 4
        case .confused: 2.15
        case .thinking: 1.9
        case .sheepish: 2.45
        case .relieved: 1.9
        case .proud: 1.8
        case .flattered: 0.4
        default: 0
        }
    }

    /// The loop after `settle`, seconds, for the shapes that keep moving until their time to live runs out.
    var period: Double? {
        switch self {
        case .laughing: 0.5      // the 2 Hz bounce
        case .sleepy: Self.nodEvery
        case .flattered: 1       // the 1 Hz sway
        case .proud: Self.twinkleEvery
        default: nil
        }
    }

    /// Which overlays this label draws (none for the board's eleven).
    var overlayKinds: Set<FaceOverlay.Kind> {
        switch self {
        case .sleepy: [.z]
        case .confused: [.question]
        case .sheepish: [.sweat]
        case .proud: [.sparkle]
        case .flattered: [.blush]
        default: []
        }
    }

    static let nodEvery = 2.5
    /// proud's sparkles twinkle again this often until the time to live runs out: once they are gone, proud is happy.
    static let twinkleEvery = 1.75
    /// Overlay sizes, board px, raised from the design's after the contact sheet at phone size (2026-10-12): a smaller
    /// mark read as a speck. Sleepy's two z's, confused's "?", sheepish's drop, proud's sparkle arm, the blush cluster.
    static let zSizes = (14.0, 20.0)
    static let questionSize = 22.0
    static let sweatSize = 15.0
    static let sparkleSize = 12.0
    static let blushSize = 20.0
    /// Gaze heights, board px, past the design's (-14 and +4) so thinking reads as up-and-away and sheepish as
    /// down-and-away even without their marks.
    static let thinkingUp = -22.0
    static let sheepishDown = 10.0

    /// The pose `t` seconds after the label showed. With Reduce Motion: the end pose held, nothing bouncing, swaying
    /// or drifting, and the overlays shown still.
    func pose(at t: Double, reduceMotion: Bool = false) -> FaceExpressionPose {
        let s = style
        var p = FaceExpressionPose(left: Double(s.left), right: Double(s.right))
        let t = max(t, 0)
        let still = reduceMotion
        switch self {
        case .laughing:
            // Both crescents bounce together: +/-4 px at 5 Hz for 1.2 s, then +/-2 px at 2 Hz. No blinks.
            p.blinks = false
            if !still {
                p.jiggle.y = t < 1.2 ? -4 * sin(2 * .pi * 5 * t) : -2 * sin(2 * .pi * 2 * (t - 1.2))
            }
        case .sleepy:
            // Flat slits; the gaze sinks 6 px; every 2.5 s the lids sag to 24 over 600 ms and pop back (nodding off).
            if still {
                p.gaze = (0, 6)
                p.overlays = [Self.z(age: 1.0, size: Self.zSizes.0, still: true),
                              Self.z(age: 0.3, size: Self.zSizes.1, still: true, start: Self.zSecondStart)]
            } else {
                p.gaze = (0, track(t, [(0, 0), (4, 6)]))
                let phase = t.truncatingRemainder(dividingBy: Self.nodEvery)
                let sag = track(phase, [(1.78, 40), (2.38, 24), (2.5, 40)])
                p.left = sag
                p.right = sag
                // Two z's rise and drift right from over the viewer's right eye, 400 ms apart, each for 2.1 s, so
                // both show together for most of the nod.
                p.overlays = [Self.z(age: phase, size: Self.zSizes.0, still: false),
                              Self.z(age: phase - Self.zGap, size: Self.zSizes.1, still: false, start: Self.zSecondStart)]
            }
        case .confused:
            // The tilt swaps twice (each side held 500 ms), the gaze sweeping with it, then rests at 100/88; a "?"
            // pops in over the pair at 200 ms and tips toward the taller eye.
            let swaps: [(Double, Double)] = [(0.5, 1), (0.62, -1), (1.0, -1), (1.12, 1), (1.5, 1), (1.62, -1), (2.0, -1)]
            if still {
                p.gaze = (0, 0)
                p.overlays = [FaceOverlay(kind: .question, anchor: .aboveCentre, size: Self.questionSize, angle: -5)]
            } else {
                // side 1: the left eye taller (104 / 80), -1: the right (80 / 104); the rest after is 100 / 88.
                p.left = track(t, [(0, 104)] + swaps.map { ($0.0, $0.1 > 0 ? 104 : 80) } + [(2.15, 100)])
                p.right = track(t, [(0, 80)] + swaps.map { ($0.0, $0.1 > 0 ? 80 : 104) } + [(2.15, 88)])
                p.gaze = (track(t, [(0, 0), (0.12, -8), (0.5, -8), (0.62, 8), (1.0, 8), (1.12, -8), (1.5, -8),
                                   (1.62, 8), (2.0, 8), (2.15, 0)]), 0)
                let pop = track(t, [(0.2, 0), (0.32, 1.15), (0.42, 1)])
                let angle = track(t, [(0, -10)] + swaps.map { ($0.0, -10 * $0.1) } + [(2.15, -5)])
                p.overlays = [FaceOverlay(kind: .question, anchor: .aboveCentre, size: Self.questionSize, scale: pop,
                                          angle: angle)]
            }
        case .thinking:
            // Up and to the viewer's left, held 1.6 s, then one slow 300 ms saccade up and right, held. No blinks
            // during the holds.
            if still {
                p.gaze = (16, Self.thinkingUp)
                p.blinks = false
            } else {
                p.gaze = (track(t, [(0, 0), (0.25, -16), (1.6, -16), (1.9, 16)]), track(t, [(0, 0), (0.25, Self.thinkingUp)]))
                p.blinks = t >= 1.6 && t < 1.9
            }
        case .sheepish:
            // Eyes to the side and a little down, two 2 px shuffles in the first second, a 300 ms glance back to
            // centre at 2 s; a sweat drop slides 6 px down the viewer's right eye's outer corner.
            if still {
                p.gaze = (14, Self.sheepishDown)
                p.overlays = [FaceOverlay(kind: .sweat, anchor: .outerTop(right: true), dy: 6, size: Self.sweatSize)]
            } else {
                let away = track(t, [(0, 0), (0.25, 1), (2.0, 1), (2.12, 0), (2.3, 0), (2.45, 1)])
                p.gaze = (14 * away, Self.sheepishDown * away)
                if t < 1 { p.jiggle.x = 2 * sin(2 * .pi * 2 * t) }
                p.overlays = [FaceOverlay(kind: .sweat, anchor: .outerTop(right: true), dy: track(t, [(0.15, 0), (1.15, 6)]),
                                          size: Self.sweatSize, scale: track(t, [(0, 0), (0.15, 1)]))]
            }
        case .relieved:
            // The exhale: both eyes shut into the arched line and reopen over 700 ms, rising 4 px as they open; then
            // the pair sinks 6 px and holds.
            if still {
                p.gaze = (0, 6)
            } else {
                p.closed = track(t, [(0, 0), (0.25, 1), (0.35, 1), (0.7, 0)])
                p.gaze = (0, track(t, [(0, 0), (0.35, 4), (0.7, 0), (0.9, 0), (1.9, 6)]))
                p.blinks = t >= 0.7
            }
        case .proud:
            // A single chin-up nudge in the first 300 ms, the gaze lifts straight up 8 px; two sparkles at the outer
            // top corners grow in, hold about a second and go, and twinkle again every 1.75 s until the time to live
            // runs out; one deliberate 300 ms blink at 1.5 s.
            if still {
                p.gaze = (0, -8)
                p.overlays = [Self.sparkle(right: false, scale: 1), Self.sparkle(right: true, scale: 1)]
            } else {
                if t < 0.3 { p.jiggle.y = -4 * sin(.pi * t / 0.3) }
                p.gaze = (0, track(t, [(0.1, 0), (0.5, -8)]))
                let grow = t < 0.15 ? 0 : track((t - 0.15).truncatingRemainder(dividingBy: Self.twinkleEvery),
                                                [(0, 0), (0.2, 1), (1.2, 1), (1.4, 0)])
                p.overlays = [Self.sparkle(right: false, scale: grow), Self.sparkle(right: true, scale: grow)]
                p.shut = track(t, [(1.5, 0), (1.6, 1), (1.7, 1), (1.8, 0)])
                p.blinks = t >= 1.8
            }
        case .flattered:
            // The gaze drops 8 px, a shy 3 px sway at 1 Hz, and "///" blush strokes under each eye.
            if still {
                p.gaze = (0, 8)
                p.overlays = [Self.blush(right: false, scale: 1), Self.blush(right: true, scale: 1)]
            } else {
                p.gaze = (0, track(t, [(0, 0), (0.4, 8)]))
                p.jiggle.x = 3 * sin(2 * .pi * t)
                let grow = track(t, [(0, 0), (0.3, 1)])
                p.overlays = [Self.blush(right: false, scale: grow), Self.blush(right: true, scale: grow)]
            }
        case .calm, .happy, .curious, .affection, .surprised, .sad, .worried, .skeptical, .frustrated, .excited, .wink:
            break
        }
        return p
    }

    /// How far apart sleepy's two z's start, and how long each rises. The second ends as the nod's loop does.
    static let zGap = 0.4
    static let zLife = 2.1
    /// The second z starts this far right of the first (board px), so the two never sit on each other.
    static let zSecondStart = 16.0

    /// One sleepy "z", `age` seconds into its rise (outside it, scale 0): up 22 px and right 12 px.
    private static func z(age: Double, size: Double, still: Bool, start: Double = 0) -> FaceOverlay {
        let life = zLife
        let a = min(max(age, 0), life) / life
        let scale = still ? 1 : (age < 0 || age > life ? 0 : track(age, [(0, 0), (0.2, 1), (1.6, 1), (life, 0)]))
        return FaceOverlay(kind: .z, anchor: .aboveRightEye, dx: start + 12 * a, dy: -22 * a, size: size, scale: scale)
    }

    private static func sparkle(right: Bool, scale: Double) -> FaceOverlay {
        FaceOverlay(kind: .sparkle, anchor: .outerTop(right: right), size: sparkleSize, scale: scale)
    }

    private static func blush(right: Bool, scale: Double) -> FaceOverlay {
        // On the outer, lower cheek, 16 px out from under the eye's middle.
        FaceOverlay(kind: .blush, anchor: .under(right: right), dx: right ? 16 : -16, dy: 4, size: blushSize, scale: scale)
    }
}

/// A value along timed keys: held before the first and after the last, eased (smoothstep) between neighbours.
func track(_ t: Double, _ keys: [(Double, Double)]) -> Double {
    guard let first = keys.first else { return 0 }
    if t <= first.0 { return first.1 }
    for (a, b) in zip(keys, keys.dropFirst()) where t < b.0 {
        let span = b.0 - a.0
        guard span > 0 else { return b.1 }
        let u = (t - a.0) / span
        return a.1 + (b.1 - a.1) * u * u * (3 - 2 * u)
    }
    return keys[keys.count - 1].1
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

    /// The firmware's parse (data.h): a nonzero uint32 id; `clear: true`, or a known label; ttl_ms a uint32, or the
    /// label's own (4000 ms for the board's eleven). Anything else is nil and does nothing.
    static func decode(_ obj: [String: Any]) -> FaceExpressionRequest? {
        guard let id = whole(obj["id"]), id != 0 else { return nil }
        let clear = flag(obj["clear"])
        let label = (obj["label"] as? String).flatMap(FaceExpressionLabel.init(rawValue:))
        guard clear || label != nil else { return nil }
        return FaceExpressionRequest(id: UInt32(id), label: clear ? nil : label,
                                     ttlMs: whole(obj["ttl_ms"]).map { Double($0) } ?? label?.style.ttlMs ?? defaultTTLms,
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
