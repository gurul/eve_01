// The face at the centre of voice mode (docs/voice-mode.md): StackChan's eyes, as the robot drew them with FluxGarage
// RoboEyes (firmware/claude_pet_stackchan/src/eyes.cpp), on the phone. Owner, 2026-10-09: "can the hold to talk be
// the stackchan face with captions". Two rounded white eyes on black that blink and glance about at rest, open and
// look at the owner while they hold to talk, look up and dart while eve thinks, and bounce with eve's voice.
// The motion is critically damped springs (VoiceLevels.swift), so nothing jumps between frames.
// eve's expressions (owner, 2026-10-12: "can u route the expressions into the eyes") draw over that: the board's shape
// for the label (Shared/FaceExpression.swift, from expression.h), with RoboEyes' lids and the wink, eased in and out.
// The 2026-10-12 round (docs/expressions-design.json) adds a level lid, a droop over a smile, each label's own motion
// (FaceExpressionLabel.pose, pure in the seconds since it showed) and small white marks: z's, a "?", a sweat drop,
// sparkles and blush strokes.
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
    /// Room for the widest open (1.16), the furthest glance and the marks over the eyes, so nothing is clipped.
    static let margin = CGSize(width: 34, height: 36)

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var motion = FaceMotion()

    var body: some View {
        let size = CGSize(width: eye.width * 2 + Self.gap + Self.margin.width * 2,
                          height: eye.height * 1.2 + Self.margin.height * 2)
        TimelineView(.animation) { context in
            let f = motion.step(now: context.date.timeIntervalSinceReferenceDate, mood: mood, level: level,
                                expression: expression, reduceMotion: reduceMotion,
                                px: eye.height / FaceExpressionStyle.boardEye)
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
        // The level lid keeps each eye's bottom where the rest eye's is: the top comes down instead.
        let rects = heights.enumerated().map { i, h in
            let side = i == 0 ? -1.0 : 1.0
            let sink = f.flat * max(eye.height - h, 0) / 2
            return CGRect(x: cx + side * (gap / 2 + w / 2) - w / 2, y: cy - h / 2 + sink, width: w, height: h)
        }
        gc.drawLayer { layer in
            for (i, side) in [-1.0, 1.0].enumerated() {
                let h = heights[i]
                let rect = rects[i]
                // The wink: once the viewer's right eye has collapsed (12 board px, as eyes.cpp), it is a curved lid,
                // with a lifted brow over the open eye.
                if side > 0, f.wink > 0.5, h <= 12 * px {
                    layer.stroke(winkLid(over: rect, px: px), with: .color(.white), style: line)
                    layer.stroke(winkBrow(over: rects[0], px: px), with: .color(.white), style: line)
                    continue
                }
                // Relieved's exhale: both eyes shut into the same arched line.
                if f.closed > 0.5, h <= 12 * px {
                    layer.stroke(winkLid(over: rect, px: px), with: .color(.white), style: line)
                    continue
                }
                // A smile past the board's full one (laughing) arches the eye's top and the lid together, so what is
                // left is an even upturned crescent, not a cap on two legs. The eleven never go past 1.
                let arch = min(max((f.smile - 1) / 0.4, 0), 1)
                let corner = min(Self.corner * f.corner, h / 2, w / 2)
                let arched = min(corner + (w / 2 - corner) * arch, h / 2)
                // The level lid flattens the top, with a small 7 board px round at its ends so it reads as a lid,
                // not a cropped box.
                let lidRound = min(7 * px, arched)
                let top = arched + (lidRound - arched) * min(max(f.flat, 0), 1)
                layer.fill(UnevenRoundedRectangle(topLeadingRadius: top, bottomLeadingRadius: corner,
                                                  bottomTrailingRadius: corner, topTrailingRadius: top,
                                                  style: .continuous).path(in: rect),
                           with: .color(.white))
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
                    let lidCorner = min(corner + (lid.width / 2 - corner) * arch, lid.height / 2)
                    layer.fill(Path(roundedRect: lid, cornerRadius: lidCorner, style: .continuous), with: .color(.black))
                }
                layer.blendMode = .normal
            }
        }
        for mark in f.overlays where mark.scale > 0.01 {
            drawOverlay(mark, eyes: (rects[0], rects[1]), px: px, in: &gc, size: canvas)
        }
    }

    /// One white mark beside the eyes (FaceOverlay), placed from its anchor, kept inside the canvas.
    static func drawOverlay(_ mark: FaceOverlay, eyes: (left: CGRect, right: CGRect), px: CGFloat,
                            in gc: inout GraphicsContext, size canvas: CGSize) {
        let (l, r) = eyes
        var at: CGPoint
        switch mark.anchor {
        case .aboveRightEye: at = CGPoint(x: r.midX + r.width / 4, y: r.minY - 12 * px)
        case .aboveCentre: at = CGPoint(x: (l.midX + r.midX) / 2, y: min(l.minY, r.minY) - 16 * px)
        case .outerTop(let right):
            at = right ? CGPoint(x: r.maxX + 7 * px, y: r.minY + 6 * px) : CGPoint(x: l.minX - 7 * px, y: l.minY + 6 * px)
            if mark.kind == .sparkle { at.y -= 10 * px; at.x += (right ? 4 : -4) * px }
        case .under(let right):
            let eye = right ? r : l
            at = CGPoint(x: eye.midX, y: eye.maxY + 4 * px)
        }
        let size = mark.size * px * mark.scale * (mark.kind == .sparkle ? 2 : 1)
        at.x = min(max(at.x + mark.dx * px, size), canvas.width - size)
        at.y = min(max(at.y + mark.dy * px, size), canvas.height - size)
        var g = gc
        g.translateBy(x: at.x, y: at.y)
        g.rotate(by: .degrees(mark.angle))
        let white = GraphicsContext.Shading.color(.white)
        let half = size / 2
        switch mark.kind {
        case .z:
            var p = Path()
            p.move(to: CGPoint(x: -half * 0.8, y: -half))
            p.addLine(to: CGPoint(x: half * 0.8, y: -half))
            p.addLine(to: CGPoint(x: -half * 0.8, y: half))
            p.addLine(to: CGPoint(x: half * 0.8, y: half))
            g.stroke(p, with: white, style: StrokeStyle(lineWidth: max(size * 0.2, 1), lineCap: .round, lineJoin: .round))
        case .question:
            let text = g.resolve(Text("?").font(.system(size: size * 1.45, weight: .heavy, design: .rounded))
                .foregroundStyle(Color.white))
            g.draw(text, at: .zero, anchor: .center)
        case .sweat:
            // A drop: round at the bottom, drawn to a point at the top.
            let rad = half * 0.62
            var p = Path()
            p.move(to: CGPoint(x: 0, y: -half))
            p.addCurve(to: CGPoint(x: rad, y: half - rad), control1: CGPoint(x: rad * 0.3, y: -half * 0.4),
                       control2: CGPoint(x: rad, y: 0))
            p.addArc(center: CGPoint(x: 0, y: half - rad), radius: rad, startAngle: .zero, endAngle: .degrees(180),
                     clockwise: false)
            p.addCurve(to: CGPoint(x: 0, y: -half), control1: CGPoint(x: -rad, y: 0),
                       control2: CGPoint(x: -rad * 0.3, y: -half * 0.4))
            g.fill(p, with: white)
        case .sparkle:
            // Four points, the sides drawn in toward the centre; `size` is each arm, so it spans twice that.
            var p = Path()
            let pinch = half * 0.22
            p.move(to: CGPoint(x: 0, y: -half))
            p.addQuadCurve(to: CGPoint(x: half, y: 0), control: CGPoint(x: pinch, y: -pinch))
            p.addQuadCurve(to: CGPoint(x: 0, y: half), control: CGPoint(x: pinch, y: pinch))
            p.addQuadCurve(to: CGPoint(x: -half, y: 0), control: CGPoint(x: -pinch, y: pinch))
            p.addQuadCurve(to: CGPoint(x: 0, y: -half), control: CGPoint(x: -pinch, y: -pinch))
            g.fill(p, with: white)
        case .blush:
            // "///": three diagonal strokes, 3 board px thick, spread across the cheek.
            var p = Path()
            let step = size * 0.55
            for i in -1...1 {
                let x = CGFloat(i) * step
                p.move(to: CGPoint(x: x - half * 0.35, y: half * 0.45))
                p.addLine(to: CGPoint(x: x + half * 0.35, y: -half * 0.45))
            }
            g.stroke(p, with: white, style: StrokeStyle(lineWidth: max(3 * px * min(mark.scale, 1), 0.5), lineCap: .round))
        }
    }

    /// A lid over the top of `eye`: its top edge, and one top corner (`x`) pulled down `depth`.
    private static func triangle(_ eye: CGRect, down x: CGFloat, by depth: CGFloat) -> Path {
        var p = Path()
        p.move(to: CGPoint(x: eye.minX - 1, y: eye.minY - 1))
        p.addLine(to: CGPoint(x: eye.maxX + 1, y: eye.minY - 1))
        // 1 pt past the eye's edge, like the top two, so no sliver of the eye is left beside the lid.
        p.addLine(to: CGPoint(x: x <= eye.midX ? x - 1 : x + 1, y: eye.minY + depth))
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
    /// The level lid, 0...1 (FaceLid.flat): top corners squared, the top lowered.
    var flat = 0.0
    /// Both eyes shut into the arched line, 0...1 (relieved's exhale).
    var closed = 0.0
    /// The expression's white marks (FaceOverlay), already scaled in or out.
    var overlays: [FaceOverlay] = []
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
    private var flat = CriticalSpring()
    private var wink = FaceWink()
    // The expression's motion (FaceExpressionLabel.pose): its height changes, its gaze, its quick moves.
    private var poseLeft = CriticalSpring()
    private var poseRight = CriticalSpring()
    private var gazeX = CriticalSpring()
    private var gazeY = CriticalSpring()
    private var jiggleX = CriticalSpring()
    private var jiggleY = CriticalSpring()
    /// The scripted closure (proud's blink, relieved's exhale) and relieved's closed line, eased like every other
    /// channel, so an expression cut off while the eyes are shut reopens them over a few frames, not in one.
    private var scripted = CriticalSpring()
    private var closedLine = CriticalSpring()
    /// When each shown id first appeared, so its motion runs from then (and does not replay when it shows again).
    private var shownID: UInt32?
    private var shownAt = 0.0
    /// The last marks drawn, and how far in they are: they shrink away when the expression ends.
    private var marks: [FaceOverlay] = []
    private var marksIn = CriticalSpring()
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

    /// One frame at `now`. `px` is points per board pixel (StackChanFace.eye's height over 96), for the motion's
    /// board-pixel moves.
    func step(now: Double, mood: FaceMood, level raw: Double, expression: FaceExpressionShown? = nil,
              reduceMotion: Bool, px: Double = 92.0 / 96) -> FaceFrame {
        let dt = last.map { min(max(now - $0, 0), 1.0 / 20) } ?? 0
        if last == nil {
            nextBlink = now + pick(Self.blinkEvery)
            nextGlance = now + pick(Self.glanceEvery)
        }
        last = now
        let level = min(max(raw, 0), 1)
        let style = expression?.label.style ?? .calm
        let winking = wink.step(expression, now: now)
        if let expression, expression.id != shownID {
            shownID = expression.id
            shownAt = now
        }
        let pose = expression.map { $0.label.pose(at: now - shownAt, reduceMotion: reduceMotion) }

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
        // An expression that points the gaze owns it: the mood's glances wait.
        let gaze = pose?.gaze
        let moodTarget: (x: Double, y: Double) = gaze == nil ? target : (0, 0)

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
        let shutFor = expression == nil ? Self.blinkShut : style.blinkShut
        if !reduceMotion, mood != .muted, !winking {
            if now >= nextBlink {
                // A shape may hold its blinks (laughing, proud's and relieved's holds): the blink waits for the next chance.
                if pose?.blinks ?? true { blinkUntil = now + shutFor }
                nextBlink = now + pick(expression == nil ? Self.blinkEvery : style.blinkEvery)
            }
            if now < blinkUntil { openTo = 0.04 }
        }

        x.step(to: moodTarget.x, dt: dt, omega: mood == .thinking && gaze == nil ? 26 : 14)
        y.step(to: moodTarget.y + bounce, dt: dt, omega: 18)
        // A blink shuts fast; sleepy's 400 ms one at a matching slow pace.
        open.step(to: openTo, dt: dt, omega: now < blinkUntil ? 60 * Self.blinkShut / shutFor : 16)
        wide.step(to: wideTo, dt: dt, omega: 12)

        // The expression's shape eases in and out (about a third of a second); with Reduce Motion it changes at once.
        func ease(_ s: inout CriticalSpring, to value: Double, omega: Double) {
            if reduceMotion { s = CriticalSpring(value: value) } else { s.step(to: value, dt: dt, omega: omega) }
        }
        // A shape that must read at once (laughing's crescent) snaps in faster.
        let shapeOmega = style.easeIn
        ease(&shapeLeft, to: style.leftOpen, omega: shapeOmega)
        ease(&shapeRight, to: style.rightOpen, omega: shapeOmega)
        ease(&shapeWide, to: style.wide, omega: shapeOmega)
        ease(&shapeCorner, to: style.corner, omega: shapeOmega)
        ease(&smile, to: style.smile, omega: shapeOmega)
        ease(&droop, to: style.droop, omega: shapeOmega)
        ease(&brow, to: style.brow, omega: shapeOmega)
        ease(&flat, to: style.flat, omega: shapeOmega)
        // The motion: height changes and quick moves follow closely (they are smooth already); the gaze eases.
        ease(&poseLeft, to: pose.map { $0.left - Double(style.left) } ?? 0, omega: 40)
        ease(&poseRight, to: pose.map { $0.right - Double(style.right) } ?? 0, omega: 40)
        ease(&gazeX, to: (gaze?.x ?? 0) * px, omega: 22)
        ease(&gazeY, to: (gaze?.y ?? 0) * px, omega: 22)
        ease(&jiggleX, to: (pose?.jiggle.x ?? 0) * px, omega: 70)
        ease(&jiggleY, to: (pose?.jiggle.y ?? 0) * px, omega: 70)
        if let pose, !pose.overlays.isEmpty { marks = pose.overlays }
        ease(&marksIn, to: pose?.overlays.isEmpty == false ? 1 : 0, omega: 16)
        if marksIn.value < 0.01, pose?.overlays.isEmpty != false { marks = [] }
        // Curious: the outer eye grows (8 board px) as the pair looks to its side; not during a wink.
        let curious = style.curious && !winking
        ease(&liftLeft, to: curious ? FaceExpressionStyle.curiousGrow * min(max(-x.value / 16, 0), 1) : 0, omega: 14)
        ease(&liftRight, to: curious ? FaceExpressionStyle.curiousGrow * min(max(x.value / 16, 0), 1) : 0, omega: 14)
        ease(&winkShut, to: winking ? 1 : 0, omega: winking ? 36 : 20)

        // A scripted blink (proud) and relieved's exhale shut both eyes; the wink shuts the right one.
        let opened = max(open.value, 0.04)
        let unit = { (v: Double) in min(max(v, 0), 1) }
        ease(&scripted, to: unit(max(pose?.shut ?? 0, pose?.closed ?? 0)), omega: 40)
        ease(&closedLine, to: unit(pose?.closed ?? 0), omega: 40)
        let closed = unit(closedLine.value)
        let shut = 1 - unit(scripted.value)
        let board = FaceExpressionStyle.boardEye
        let fade = unit(marksIn.value)
        return FaceFrame(x: x.value + gazeX.value + jiggleX.value, y: y.value + gazeY.value + jiggleY.value,
                         open: opened, wide: wide.value * shapeWide.value,
                         left: max(opened * (shapeLeft.value + poseLeft.value / board + liftLeft.value) * shut, 0.04),
                         right: max(opened * (shapeRight.value + poseRight.value / board + liftRight.value) * shut
                                    * (1 - unit(winkShut.value)), 0.04),
                         corner: shapeCorner.value,
                         smile: min(max(smile.value, 0), 1.5), droop: unit(droop.value),
                         brow: unit(brow.value), wink: unit(winkShut.value), flat: unit(flat.value), closed: closed,
                         overlays: marks.map { var m = $0; m.scale *= fade; return m })
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
