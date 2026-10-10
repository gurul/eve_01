// Eve Link's widget extension: the "Talk to eve" control (Control Center, the Lock Screen and Settings > Action
// Button > Controls) and the call's Live Activity, both running TalkIntent, which the system performs in the app; and
// the home-screen widget (TodayWidget.swift, docs/widget.md).
import ActivityKit
import AppIntents
import SwiftUI
import WidgetKit

@main
struct EveLinkControls: WidgetBundle {
    var body: some Widget {
        TalkControl()
        TalkLiveActivity()
        TodayWidget()
    }
}

struct TalkControl: ControlWidget {
    static let kind = "com.github.eve-bridge.EveLink.talk"

    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: Self.kind) {
            ControlWidgetButton(action: TalkIntent()) {
                Label("Talk to eve", systemImage: "waveform")
                    .controlWidgetActionHint("Hold to Talk")
            }
        }
        .displayName("Talk to eve")
        .description("Hold to open the microphone. Stop talking, or press again, to send.")
    }
}

/// Minimal, like the app icon (owner, 2026-10-08: "everything should be the minimalist type"): white on black, eve's
/// two white pill eyes, one line of words, plain symbols. The one colour is a small red dot while notes record, since
/// the microphone stays open until a press and must not look like an ordinary turn.
private enum Look {
    static let ink = Color.white
    static let quiet = Color.white.opacity(0.55)
    static let screen = Color.black
    static let recording = Color(red: 1, green: 69 / 255, blue: 58 / 255)

    static func subtitle(_ state: TalkActivityAttributes.ContentState) -> String {
        if !state.heard.isEmpty, state.phase != .listening, state.phase != .notes { return "\u{201C}\(state.heard)\u{201D}" }
        switch state.phase {
        case .listening: return "Pause or press to send"
        case .thinking: return "Thinking"
        case .speaking: return "Press to cut in"
        case .waiting: return "Hold to talk"
        case .notes: return "Press to stop"
        case .ended: return ""
        }
    }

    /// The press button's symbol: what a press does now.
    static func symbol(_ phase: CallPhase) -> String {
        switch phase {
        case .listening: "arrow.up"
        case .speaking, .notes: "stop.fill"
        default: "mic.fill"
        }
    }
}

/// eve's face: two white pill eyes. They narrow while eve thinks and dim while nobody talks.
private struct Eyes: View {
    let phase: CallPhase
    var size: CGFloat = 14

    var body: some View {
        HStack(spacing: size * 0.42) {
            eye
            eye
        }
        .opacity(phase == .waiting || phase == .ended ? 0.6 : 1)
        .accessibilityHidden(true)
    }

    private var eye: some View {
        Capsule()
            .fill(Look.ink)
            .frame(width: size, height: phase == .thinking ? size * 0.24 : size * 0.5)
    }
}

/// The right-hand status: a running timer while listening or taking notes, a quiet symbol otherwise. Always one line.
private struct Status: View {
    let state: TalkActivityAttributes.ContentState
    var timerWidth: CGFloat = 40

    var body: some View {
        switch state.phase {
        case .listening, .notes:
            HStack(spacing: 4) {
                if state.phase == .notes {
                    Circle().fill(Look.recording).frame(width: 6, height: 6)
                }
                Text(timerInterval: state.since...Date.distantFuture, countsDown: false)
                    .monospacedDigit()
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
                    .multilineTextAlignment(.trailing)
                    .frame(width: state.phase == .notes ? timerWidth + 14 : timerWidth, alignment: .trailing)
            }
            .foregroundStyle(Look.ink)
        case .thinking:
            Image(systemName: "ellipsis")
                .symbolEffect(.variableColor.iterative, options: .repeating)
                .foregroundStyle(Look.ink)
        case .speaking:
            Image(systemName: "waveform")
                .symbolEffect(.variableColor.iterative, options: .repeating)
                .foregroundStyle(Look.ink)
        case .waiting, .ended:
            Image(systemName: "waveform").foregroundStyle(Look.quiet)
        }
    }
}

/// One round button: what a press does now (talk, send, stop).
private struct PressButton: View {
    let phase: CallPhase

    var body: some View {
        Button(intent: TalkIntent()) {
            Image(systemName: Look.symbol(phase))
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(Look.screen)
                .frame(width: 44, height: 44)
                .background(Circle().fill(Look.ink))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(phase.nextPress)
    }
}

private struct EndButton: View {
    var body: some View {
        Button(intent: EndCallIntent()) {
            Image(systemName: "xmark")
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(Look.ink)
                .frame(width: 44, height: 44)
                .background(Circle().fill(Color.white.opacity(0.14)))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("End the call")
    }
}

/// Title and subtitle, one line each.
private struct Words: View {
    let state: TalkActivityAttributes.ContentState
    var alignment: HorizontalAlignment = .leading

    var body: some View {
        VStack(alignment: alignment, spacing: 1) {
            Text(state.phase.title)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(Look.ink)
                .lineLimit(1)
            let sub = Look.subtitle(state)
            if !sub.isEmpty {
                Text(sub)
                    .font(.system(size: 13))
                    .foregroundStyle(Look.quiet)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
        }
    }
}

struct TalkLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: TalkActivityAttributes.self) { context in
            // The Lock Screen and the banner: one row.
            let state = context.state
            HStack(spacing: 14) {
                Eyes(phase: state.phase, size: 16)
                    .frame(width: 44)
                Words(state: state)
                Spacer(minLength: 4)
                Status(state: state).font(.system(size: 15, weight: .medium))
                PressButton(phase: state.phase)
                EndButton()
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
            .activityBackgroundTint(Look.screen)
            .activitySystemActionForegroundColor(Look.ink)
        } dynamicIsland: { context in
            let state = context.state
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Eyes(phase: state.phase, size: 14)
                        .padding(.leading, 8)
                        .frame(maxHeight: .infinity)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Status(state: state)
                        .font(.system(size: 15, weight: .medium))
                        .padding(.trailing, 8)
                        .frame(maxHeight: .infinity)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    HStack(spacing: 12) {
                        Words(state: state)
                        Spacer(minLength: 4)
                        PressButton(phase: state.phase)
                        EndButton()
                    }
                    .padding(.horizontal, 8)
                    .padding(.top, 2)
                }
            } compactLeading: {
                Eyes(phase: state.phase, size: 8)
                    .padding(.leading, 4)
            } compactTrailing: {
                Status(state: state, timerWidth: 34)
                    .font(.system(size: 13, weight: .semibold))
                    .padding(.trailing, 2)
            } minimal: {
                Eyes(phase: state.phase, size: 6)
            }
            .keylineTint(Look.ink)
        }
    }
}
