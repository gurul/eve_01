// Voice mode (docs/voice-mode.md): black, StackChan's face with captions under it, two round buttons on top, and one
// row at the bottom. Shown with
// .fullScreenCover; the host owns the call, this view only draws it and reports taps.
import SwiftUI
import UIKit

struct VoiceModeView: View {
    /// The speed-dial choices: (value stored in `grade`, label shown).
    static let grades: [(value: String, label: String)] = [("auto", "Auto"), ("instant", "Instant"), ("think", "Think")]
    /// The face and its caption: the block that is held to talk (VoiceCall), and how far below centre it sits so
    /// the eyes stay near the screen's middle with the caption under them.
    static let talkArea = CGSize(width: 320, height: 330)
    static let faceOffset: CGFloat = 40
    /// The bottom row's height and its round buttons' diameter.
    private static let rowHeight: CGFloat = 48

    let levels: VoiceLevels
    @Binding var grade: String
    let muted: Bool
    let onMenu: () -> Void
    let onSettings: () -> Void
    let onAskText: () -> Void
    let onMute: () -> Void
    let onClose: () -> Void

    init(levels: VoiceLevels, grade: Binding<String>, muted: Bool,
         onMenu: @escaping () -> Void, onSettings: @escaping () -> Void, onAskText: @escaping () -> Void,
         onMute: @escaping () -> Void, onClose: @escaping () -> Void) {
        self.levels = levels
        _grade = grade
        self.muted = muted
        self.onMenu = onMenu
        self.onSettings = onSettings
        self.onAskText = onAskText
        self.onMute = onMute
        self.onClose = onClose
    }

    @State private var listeningShown = true
    @Environment(\.displayScale) private var displayScale

    private static let surface = Color(white: 0.11)
    private static let hairline = Color.white.opacity(0.09)

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            LiveFace(levels: levels, muted: muted)
            VStack(spacing: 0) {
                topBar
                Spacer(minLength: 0)
                statusLine
                    .padding(.bottom, 14)
                bottomRow
                    .padding(.horizontal, 20)       // the row sits in from the edges, as in the reference
            }
            .padding(.horizontal, 16)
            .padding(.top, 8)
            .padding(.bottom, 10)
        }
        .preferredColorScheme(.dark)
        .statusBarHidden(false)
        .sensoryFeedback(.impact(flexibility: .soft), trigger: levels.speaker) { old, new in
            new == .eve && old != .eve
        }
        .sensoryFeedback(.impact(weight: .light), trigger: muted)
        .task(id: levels.status) {
            // "Listening" is the normal state: say it briefly, then get out of the way.
            listeningShown = true
            guard levels.status == "Listening" else { return }
            try? await Task.sleep(for: .seconds(1.6))
            if !Task.isCancelled { listeningShown = false }
        }
    }

    // MARK: Top

    private var topBar: some View {
        HStack {
            Button(action: onMenu) {
                VStack(alignment: .leading, spacing: 5) {
                    Capsule().frame(width: 18, height: 2)
                    Capsule().frame(width: 12, height: 2)
                }
                .frame(width: 44, height: 44)
                .background(Circle().fill(Self.surface))
                .overlay(Circle().strokeBorder(Self.hairline, lineWidth: 1 / displayScale))
            }
            .accessibilityLabel("Menu")
            Spacer()
            Button(action: onSettings) {
                Image(systemName: "slider.horizontal.3")
                    .font(.system(size: 17, weight: .medium))
                    .frame(width: 44, height: 44)
                    .background(Circle().fill(Self.surface))
                    .overlay(Circle().strokeBorder(Self.hairline, lineWidth: 1 / displayScale))
            }
            .accessibilityLabel("Voice settings")
        }
        .buttonStyle(.plain)
        .foregroundStyle(.white)
    }

    // MARK: Status

    /// The line above the bottom row; empty when there is nothing worth saying.
    private var statusText: String {
        if muted { return "Muted" }
        let status = levels.status
        if status == "Listening", !listeningShown { return "" }
        return status
    }

    private var statusLine: some View {
        Text(statusText.isEmpty ? " " : statusText)
            .font(.footnote)
            .foregroundStyle(Color(white: 0.55))
            .lineLimit(2)
            .multilineTextAlignment(.center)
            .opacity(statusText.isEmpty ? 0 : 1)
            .animation(.easeInOut(duration: 0.35), value: statusText)
            .accessibilityHidden(statusText.isEmpty)
            .accessibilityAddTraits(.updatesFrequently)
    }

    // MARK: Bottom

    private var gradeLabel: String {
        Self.grades.first { $0.value == grade }?.label ?? "Auto"
    }

    private var bottomRow: some View {
        HStack(spacing: 8) {
            HStack(spacing: 0) {
                Button {} label: {
                    Image(systemName: "plus")
                        .font(.system(size: 20, weight: .regular))
                        .frame(width: 44, height: 44)
                }
                .disabled(true)
                .accessibilityLabel("Add")
                .accessibilityHint("Not available yet")

                Button(action: onAskText) {
                    Text("Ask eve")
                        .font(.system(size: 17))
                        .foregroundStyle(Color(white: 0.5))
                        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel("Ask eve")
                .accessibilityHint("Closes voice and opens the keyboard")

                Menu {
                    Picker("Speed", selection: $grade) {
                        ForEach(Self.grades, id: \.value) { Text($0.label).tag($0.value) }
                    }
                } label: {
                    Image(systemName: "gauge.with.dots.needle.33percent")
                        .font(.system(size: 19, weight: .regular))
                        .frame(width: 44, height: 44)
                }
                .menuOrder(.fixed)
                .accessibilityLabel("Speed")
                .accessibilityValue(gradeLabel)
            }
            .padding(.horizontal, 4)
            .frame(height: Self.rowHeight)
            .background(Capsule().fill(Self.surface))
            .overlay(Capsule().strokeBorder(Self.hairline, lineWidth: 1 / displayScale))

            Button(action: onMute) {
                Image(systemName: muted ? "mic.slash" : "mic")
                    .font(.system(size: 20, weight: .regular))
                    .contentTransition(.symbolEffect(.replace))
                    .frame(width: Self.rowHeight, height: Self.rowHeight)
                    .background(Circle().fill(Self.surface))
                    .overlay(Circle().strokeBorder(Self.hairline, lineWidth: 1 / displayScale))
            }
            .accessibilityLabel(muted ? "Unmute" : "Mute")

            Button {
                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                onClose()
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 20, weight: .semibold))
                    .foregroundStyle(.black)
                    .frame(width: Self.rowHeight, height: Self.rowHeight)
                    .background(Circle().fill(.white))
            }
            .accessibilityLabel("End voice")
        }
        .buttonStyle(.plain)
        .foregroundStyle(.white)
    }
}

/// Reads the levels in its own body, so ~30 level changes a second redraw only the face and its caption.
private struct LiveFace: View {
    let levels: VoiceLevels
    let muted: Bool

    var body: some View {
        let mood = Self.mood(muted: muted, speaker: levels.speaker, status: levels.status)
        VStack(spacing: 22) {
            // eve's expression shows only while he rests, thinks or speaks (FaceExpressionState.allowed).
            // While the owner talks the eyes hold still; they react once their words are transcribed (owner,
            // 2026-10-13: "eyes should start reacting when the stt happens not before"). eve's own voice still moves them.
            StackChanFace(mood: mood, level: levels.speaker == .owner ? 0 : levels.level,
                          expression: FaceExpressionState.gate(levels.expression, mood: mood))
            VoiceCaptionView(caption: levels.caption)
                .padding(.horizontal, 28)
        }
        .offset(y: VoiceModeView.faceOffset)     // the caption below keeps the eyes near the screen's centre
    }

    /// The face's mood: who is speaking wins; then muted, thinking and connecting from the status line.
    static func mood(muted: Bool, speaker: VoiceSpeaker, status: String) -> FaceMood {
        switch speaker {
        case .owner: return .listening
        case .eve: return .speaking
        case .idle: break
        }
        if muted { return .muted }
        if status.hasPrefix("eve is thinking") || status.hasPrefix("Writing up") { return .thinking }
        if status.hasPrefix("Connecting") { return .connecting }
        return .idle
    }
}

#Preview("Voice mode") {
    @Previewable @State var grade = "auto"
    @Previewable @State var muted = false
    @Previewable @State var levels = VoiceLevels()
    VoiceModeView(levels: levels, grade: $grade, muted: muted,
                  onMenu: {}, onSettings: {}, onAskText: {}, onMute: { muted.toggle() }, onClose: {})
        .task {
            // Fake turns: the owner talks, eve answers, a pause; 100 ms chunks of a wobbling tone.
            let rate = Double(PCM16.sampleRate)
            var t = 0.0
            while !Task.isCancelled {
                let cycle = t.truncatingRemainder(dividingBy: 9)
                let syllable = max(0, sin(t * 7.5)) * (0.6 + 0.4 * sin(t * 1.3))
                let amplitude = cycle < 8 ? 0.02 + 0.3 * syllable : 0
                let chunk = (0..<2400).map { i in
                    Int16(32767 * amplitude * sin(2 * .pi * 220 * (t + Double(i) / rate)))
                }
                switch cycle {
                case ..<3.5:
                    levels.status = "Listening"
                    if !muted { levels.ownerPCM(chunk) }
                    if cycle < 0.1 { levels.ownerSaid("what's on my calendar today") }
                case 3.5..<4:
                    levels.status = "eve is thinking…"
                case 4..<8:
                    levels.status = "eve is speaking"
                    if cycle < 4.1 {
                        levels.eveSaid("You have two meetings today: standup at ten and lunch.")
                        // eve's expression for the sentence: a different label each turn.
                        let labels = FaceExpressionLabel.allCases
                        let label = labels[Int(t / 9) % labels.count]
                        levels.showExpression(FaceExpressionRequest(id: UInt32(t * 10) + 1, label: label,
                                                                    ttlMs: label.style.ttlMs))
                    }
                    levels.evePCM(chunk)
                default:
                    levels.status = "Listening"
                }
                t += 0.1
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
}
