// The face at the centre of voice mode (docs/voice-mode.md): StackChan's eyes, as the robot drew them with FluxGarage
// RoboEyes (firmware/claude_pet_stackchan/src/eyes.cpp), on the phone. Owner, 2026-10-09: "can the hold to talk be
// the stackchan face with captions". Two rounded white eyes on black that blink and glance about at rest, open and
// look at the owner while they hold to talk, look up and dart while eve thinks, and bounce with eve's voice.
// The motion is critically damped springs (VoiceLevels.swift), so nothing jumps between frames.
// eve's expressions (owner, 2026-10-12: "can u route the expressions into the eyes") draw over that: the board's shape
// for the label (Shared/FaceExpression.swift, from expression.h), with RoboEyes' lids and the wink, eased in and out.
// FaceMood lives in Shared/FaceExpression.swift, so the Mac tests can check which moods may show one.
import SwiftUI

struct StackChanFace: View {
    var mood: FaceMood
    /// Current loudness, 0...1 (VoiceLevels.level): the owner's while they talk, eve's while he does.
    var level: Double
    /// eve's expression (Shared/FaceExpression.swift), already gated by the mood; nil is the mood's own face.
    var expression: FaceExpressionShown? = nil
    /// One eye's size at rest; the pair is two eyes and the gap between them.
    var eye = CGSize(width: 74, height: 92)

    static let gap: CGFloat = 46
    static let corner: CGFloat = 24
    /// Room for the widest open (1.16) and the furthest glance, so an eye is never clipped.
    static let margin = CGSize(width: 34, height: 30)

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var motion = FaceMotion()

    var body: some View {
        let size = CGSize(width: eye.width * 2 + Self.gap + Self.margin.width * 2,
                          height: eye.height * 1.2 + Self.margin.height * 2)
        TimelineView(.animation) { context in
            let f = motion.step(now: context.date.timeIntervalSinceReferenceDate, mood: mood, level: level,
                                expression: expression, reduceMotion: reduceMotion)
            Canvas { gc, canvas in
                Self.draw(f, eye: eye, in: &gc, size: canvas)
            }
        }
        .frame(width: size.width, height: size.height)
        .accessibilityElement()
        .accessibilityLabel("eve")
        .accessibilityValue(Self.spoken(mood, expression?.label))
    }

    /// Both eyes, as RoboEyes draws them: a rounded white rect each, then the expression's lids cut out of them, then
    /// the wink's closed lid and brow. Lids are erased (destinationOut) inside one layer, so they need no background.
    static func draw(_ f: FaceFrame, eye: CGSize, in gc: inout GraphicsContext, size canvas: CGSize) {
        let px = eye.height / FaceExpressionStyle.boardEye           // points per board pixel
        let w = eye.width * f.wide
        let heights = [max(eye.height * f.left, 4), max(eye.height * f.right, 4)]   // a blink is a short bar
        // The widest shape and the furthest glance together stay inside the canvas.
        let roomX = max(canvas.width / 2 - gap / 2 - w, 0)
        let roomY = max(canvas.height / 2 - max(heights[0], heights[1]) / 2, 0)
        let cx = canvas.width / 2 + min(max(f.x, -roomX), roomX)
        let cy = canvas.height / 2 + min(max(f.y, -roomY), roomY)
        let line = StrokeStyle(lineWidth: 4 * px, lineCap: .round, lineJoin: .round)
        gc.drawLayer { layer in
            var leftEye = CGRect.zero
            for (i, side) in [-1.0, 1.0].enumerated() {
                let h = heights[i]
                let rect = CGRect(x: cx + side * (gap / 2 + w / 2) - w / 2, y: cy - h / 2, width: w, height: h)
                if side < 0 { leftEye = rect }
                // The wink: once the viewer's right eye has collapsed (12 board px, as eyes.cpp), it is a curved lid,
                // with a lifted brow over the open eye.
                if side > 0, f.wink > 0.5, h <= 12 * px {
                    layer.stroke(winkLid(over: rect, px: px), with: .color(.white), style: line)
                    layer.stroke(winkBrow(over: leftEye, px: px), with: .color(.white), style: line)
                    continue
                }
                let corner = min(Self.corner * f.corner, h / 2, w / 2)
                layer.fill(Path(roundedRect: rect, cornerRadius: corner, style: .continuous), with: .color(.white))
                layer.blendMode = .destinationOut
                let outer = side < 0 ? rect.minX : rect.maxX
                let inner = side < 0 ? rect.maxX : rect.minX
                if f.droop > 0.01 {      // TIRED: the outer top corner pulled down, half the eye at most
                    layer.fill(triangle(rect, down: outer, by: f.droop * h / 2), with: .color(.black))
                }
                if f.brow > 0.01 {       // ANGRY: the inner top corner pulled down
                    layer.fill(triangle(rect, down: inner, by: f.brow * h / 2), with: .color(.black))
                }
                if f.smile > 0.01 {      // HAPPY: a rounded lid pushed up from below cuts the bottom half away
                    let lid = CGRect(x: rect.minX - 1, y: rect.maxY - f.smile * h / 2 + 1,
                                     width: rect.width + 2, height: max(h, eye.height))
                    layer.fill(Path(roundedRect: lid, cornerRadius: corner, style: .continuous), with: .color(.black))
                }
                layer.blendMode = .normal
            }
        }
    }

    /// A lid over the top of `eye`: its top edge, and one top corner (`x`) pulled down `depth`.
    private static func triangle(_ eye: CGRect, down x: CGFloat, by depth: CGFloat) -> Path {
        var p = Path()
        p.move(to: CGPoint(x: eye.minX - 1, y: eye.minY - 1))
        p.addLine(to: CGPoint(x: eye.maxX + 1, y: eye.minY - 1))
        p.addLine(to: CGPoint(x: x, y: eye.minY + depth))
        p.closeSubpath()
        return p
    }

    /// The closed eye: an arch across the eye's centre, 5 board px narrower than it (expression.h winkCurveY).
    private static func winkLid(over eye: CGRect, px: CGFloat) -> Path {
        let span = max(eye.width - 5 * px, 1)
        let start = eye.minX + (eye.width - span) / 2
        return curve(count: 32) { t in
            let x = span * t
            return CGPoint(x: start + x, y: eye.midY + FaceWink.curve(x / px, width: span / px) * px)
        }
    }

    /// The lifted brow over the open eye: 3/4 of its width, 8 board px above it, rising 10 px toward the inner end.
    private static func winkBrow(over eye: CGRect, px: CGFloat) -> Path {
        let width = eye.width * 3 / 4
        let start = eye.minX + (eye.width - width) / 2
        return curve(count: 32) { t in
            let x = width * t
            return CGPoint(x: start + x,
                           y: eye.minY - 8 * px - 10 * px * t + FaceWink.curve(x / px, width: width / px) * px / 3)
        }
    }

    private static func curve(count: Int, _ point: (CGFloat) -> CGPoint) -> Path {
        var p = Path()
        p.move(to: point(0))
        for i in 1...count { p.addLine(to: point(CGFloat(i) / CGFloat(count))) }
        return p
    }

    static func spoken(_ mood: FaceMood, _ expression: FaceExpressionLabel? = nil) -> String {
        let base = switch mood {
        case .idle: "resting"
        case .listening: "listening"
        case .thinking: "thinking"
        case .speaking: "speaking"
        case .muted: "muted"
        case .connecting: "connecting"
        }
        guard let expression else { return base }
        return "\(base), \(expression.spoken)"
    }
}

/// One frame of the face: where the pair looks (points from the centre), how open (1 = at rest) and how wide, and
/// eve's expression on top: each eye's height (open, times its shape, times the wink), the corner, and the lids.
struct FaceFrame: Equatable {
    var x: Double
    var y: Double
    var open: Double
    var wide: Double
    /// The viewer's left and right eye's height, as a share of the rest eye.
    var left = 1.0
    var right = 1.0
    /// The corner radius, as a share of StackChanFace.corner.
    var corner = 1.0
    /// How far each lid is in, 0...1 (FaceLid).
    var smile = 0.0
    var droop = 0.0
    var brow = 0.0
    /// How shut the wink is, 0...1.
    var wink = 0.0
}

/// The face's state between frames. A plain class held in @State: stepping it inside the TimelineView invalidates
/// nothing; the timeline already redraws every frame. `random` is injectable so the behaviour is testable.
@MainActor
final class FaceMotion {
    /// Seconds between blinks, and how long the lids stay shut.
    static let blinkEvery: ClosedRange<Double> = 2.6...6.0
    static let blinkShut = 0.11
    /// Seconds between glances at rest, and while thinking.
    static let glanceEvery: ClosedRange<Double> = 1.4...3.8
    static let dartEvery: ClosedRange<Double> = 0.3...0.7

    private var last: Double?
    private var x = CriticalSpring()
    private var y = CriticalSpring()
    private var open = CriticalSpring(value: 1)
    private var wide = CriticalSpring(value: 1)
    // eve's expression: each eye's shape, the width and corner, the lids, the curious eye and the wink.
    private var shapeLeft = CriticalSpring(value: 1)
    private var shapeRight = CriticalSpring(value: 1)
    private var shapeWide = CriticalSpring(value: 1)
    private var shapeCorner = CriticalSpring(value: 1)
    private var smile = CriticalSpring()
    private var droop = CriticalSpring()
    private var brow = CriticalSpring()
    private var liftLeft = CriticalSpring()
    private var liftRight = CriticalSpring()
    private var winkShut = CriticalSpring()
    private var wink = FaceWink()
    private var target = (x: 0.0, y: 0.0)
    private var nextGlance = 0.0
    private var nextBlink = 0.0
    private var blinkUntil = -1.0
    private var random: () -> Double

    init(random: @escaping () -> Double = { Double.random(in: 0...1) }) {
        self.random = random
    }

    private func pick(_ range: ClosedRange<Double>) -> Double {
        range.lowerBound + (range.upperBound - range.lowerBound) * random()
    }

    func step(now: Double, mood: FaceMood, level raw: Double, expression: FaceExpressionShown? = nil,
              reduceMotion: Bool) -> FaceFrame {
        let dt = last.map { min(max(now - $0, 0), 1.0 / 20) } ?? 0
        if last == nil {
            nextBlink = now + pick(Self.blinkEvery)
            nextGlance = now + pick(Self.glanceEvery)
        }
        last = now
        let level = min(max(raw, 0), 1)
        let style = expression?.label.style ?? .calm
        let winking = wink.step(expression, now: now)

        // Where to look: the owner (centre) while they talk; up and to one side, darting, while eve thinks; a
        // small glance now and then at rest; down when muted. Reduce Motion keeps the gaze still.
        if reduceMotion {
            target = (0, 0)
        } else if now >= nextGlance {
            switch mood {
            case .thinking:
                target = (18 + 14 * random(), -14 - 8 * random())
                nextGlance = now + pick(Self.dartEvery)
            case .idle, .connecting:
                target = random() < 0.35 ? (0, 0) : (-20 + 40 * random(), -10 + 16 * random())
                nextGlance = now + pick(Self.glanceEvery)
            default:
                nextGlance = now + pick(Self.glanceEvery)
            }
        }
        switch mood {
        case .listening, .speaking: if !reduceMotion { target = (target.x * 0.2, 0) }
        case .muted: target = (0, 8)
        default: break
        }

        // How open: wide and pulsing a little with the owner's voice; squinting while thinking; bouncing with
        // eve's voice; sleepy when muted. A blink shuts them for a moment; an expression sets its own blink pace,
        // and a wink holds still.
        var openTo: Double
        var wideTo = 1.0
        var bounce = 0.0
        switch mood {
        case .idle: openTo = 1
        case .connecting: openTo = 0.6
        case .listening:
            openTo = 1.12 + 0.06 * level
            wideTo = 1.04
        case .thinking: openTo = 0.78
        case .speaking:
            openTo = 0.9 + 0.22 * level
            bounce = reduceMotion ? 0 : -6 * level
        case .muted: openTo = 0.32
        }
        // While an expression shows, its heights are the eye (the board opens the eyes and sets the style's heights
        // over the agent face's): the mood's openness eases back to 1, keeping only the speaking bounce and the blink.
        if expression != nil {
            openTo = mood == .speaking ? 1 + 0.1 * level : 1
        }
        if let expression, nextBlink > now + expression.label.style.blinkEvery.upperBound {
            nextBlink = now + pick(style.blinkEvery)      // a quick-blinking shape does not wait out a slow pace
        }
        if !reduceMotion, mood != .muted, !winking {
            if now >= nextBlink {
                blinkUntil = now + Self.blinkShut
                nextBlink = now + pick(expression == nil ? Self.blinkEvery : style.blinkEvery)
            }
            if now < blinkUntil { openTo = 0.04 }
        }

        x.step(to: target.x, dt: dt, omega: mood == .thinking ? 26 : 14)
        y.step(to: target.y + bounce, dt: dt, omega: 18)
        open.step(to: openTo, dt: dt, omega: now < blinkUntil ? 60 : 16)
        wide.step(to: wideTo, dt: dt, omega: 12)

        // The expression's shape eases in and out (about a third of a second); with Reduce Motion it changes at once.
        func ease(_ s: inout CriticalSpring, to value: Double, omega: Double) {
            if reduceMotion { s = CriticalSpring(value: value) } else { s.step(to: value, dt: dt, omega: omega) }
        }
        ease(&shapeLeft, to: style.leftOpen, omega: 12)
        ease(&shapeRight, to: style.rightOpen, omega: 12)
        ease(&shapeWide, to: style.wide, omega: 12)
        ease(&shapeCorner, to: style.corner, omega: 12)
        ease(&smile, to: style.lid == .smile ? 1 : 0, omega: 12)
        ease(&droop, to: style.lid == .droop ? 1 : 0, omega: 12)
        ease(&brow, to: style.lid == .brow ? 1 : 0, omega: 12)
        // Curious: the outer eye grows (8 board px) as the pair looks to its side; not during a wink.
        let curious = style.curious && !winking
        ease(&liftLeft, to: curious ? FaceExpressionStyle.curiousGrow * min(max(-x.value / 16, 0), 1) : 0, omega: 14)
        ease(&liftRight, to: curious ? FaceExpressionStyle.curiousGrow * min(max(x.value / 16, 0), 1) : 0, omega: 14)
        ease(&winkShut, to: winking ? 1 : 0, omega: winking ? 36 : 20)

        let opened = max(open.value, 0.04)
        return FaceFrame(x: x.value, y: y.value, open: opened, wide: wide.value * shapeWide.value,
                         left: max(opened * (shapeLeft.value + liftLeft.value), 0.04),
                         right: max(opened * (shapeRight.value + liftRight.value) * (1 - min(max(winkShut.value, 0), 1)), 0.04),
                         corner: shapeCorner.value,
                         smile: min(max(smile.value, 0), 1), droop: min(max(droop.value, 0), 1),
                         brow: min(max(brow.value, 0), 1), wink: min(max(winkShut.value, 0), 1))
    }
}

/// The words under the face: eve's sentence as it plays (white), or what eve heard the owner say (grey).
struct VoiceCaptionView: View {
    var caption: VoiceCaption?

    var body: some View {
        ZStack {
            if let caption {
                Text(caption.text)
                    .font(caption.who == .eve ? .title3.weight(.medium) : .body)
                    .foregroundStyle(caption.who == .eve ? Color.white : Color(white: 0.55))
                    .multilineTextAlignment(.center)
                    .lineLimit(5)
                    .minimumScaleFactor(0.75)
                    .id(caption)
                    .transition(.opacity)
                    .accessibilityLabel(caption.who == .eve ? "eve: \(caption.text)" : "You: \(caption.text)")
                    .accessibilityAddTraits(.updatesFrequently)
            }
        }
        .frame(maxWidth: .infinity, minHeight: 132, maxHeight: 132, alignment: .top)
        .animation(.easeInOut(duration: 0.25), value: caption)
    }
}
