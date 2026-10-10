// The face at the centre of voice mode (docs/voice-mode.md): StackChan's eyes, as the robot drew them with FluxGarage
// RoboEyes (firmware/claude_pet_stackchan/src/eyes.cpp), on the phone. Owner, 2026-10-09: "can the hold to talk be
// the stackchan face with captions". Two rounded white eyes on black that blink and glance about at rest, open and
// look at the owner while they hold to talk, look up and dart while eve thinks, and bounce with eve's voice.
// The motion is critically damped springs (VoiceLevels.swift), so nothing jumps between frames.
import SwiftUI

/// What the face is doing, from the call's state (VoiceModeView.faceMood).
enum FaceMood: Sendable, Equatable {
    case idle, listening, thinking, speaking, muted, connecting
}

struct StackChanFace: View {
    var mood: FaceMood
    /// Current loudness, 0...1 (VoiceLevels.level): the owner's while they talk, eve's while he does.
    var level: Double
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
                                reduceMotion: reduceMotion)
            Canvas { gc, canvas in
                let h = max(eye.height * f.open, 4)          // a blink is a short bar, as RoboEyes draws it
                let w = eye.width * f.wide
                let cx = canvas.width / 2 + f.x
                let cy = canvas.height / 2 + f.y
                for side in [-1.0, 1.0] {
                    let x = cx + side * (Self.gap / 2 + w / 2) - w / 2
                    let rect = CGRect(x: x, y: cy - h / 2, width: w, height: h)
                    let corner = min(Self.corner, h / 2, w / 2)
                    gc.fill(Path(roundedRect: rect, cornerRadius: corner, style: .continuous), with: .color(.white))
                }
            }
        }
        .frame(width: size.width, height: size.height)
        .accessibilityElement()
        .accessibilityLabel("eve")
        .accessibilityValue(Self.spoken(mood))
    }

    static func spoken(_ mood: FaceMood) -> String {
        switch mood {
        case .idle: "resting"
        case .listening: "listening"
        case .thinking: "thinking"
        case .speaking: "speaking"
        case .muted: "muted"
        case .connecting: "connecting"
        }
    }
}

/// One frame of the face: where the pair looks (points from the centre), how open (1 = at rest) and how wide.
struct FaceFrame: Equatable {
    var x: Double
    var y: Double
    var open: Double
    var wide: Double
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

    func step(now: Double, mood: FaceMood, level raw: Double, reduceMotion: Bool) -> FaceFrame {
        let dt = last.map { min(max(now - $0, 0), 1.0 / 20) } ?? 0
        if last == nil {
            nextBlink = now + pick(Self.blinkEvery)
            nextGlance = now + pick(Self.glanceEvery)
        }
        last = now
        let level = min(max(raw, 0), 1)

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
        // eve's voice; sleepy when muted. A blink shuts them for a moment.
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
        if !reduceMotion, mood != .muted {
            if now >= nextBlink {
                blinkUntil = now + Self.blinkShut
                nextBlink = now + pick(Self.blinkEvery)
            }
            if now < blinkUntil { openTo = 0.04 }
        }

        x.step(to: target.x, dt: dt, omega: mood == .thinking ? 26 : 14)
        y.step(to: target.y + bounce, dt: dt, omega: 18)
        open.step(to: openTo, dt: dt, omega: now < blinkUntil ? 60 : 16)
        wide.step(to: wideTo, dt: dt, omega: 12)
        return FaceFrame(x: x.value, y: y.value, open: max(open.value, 0.04), wide: wide.value)
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
