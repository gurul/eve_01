// "Talk to eve": the one intent behind the Action Button, the Control, the App Shortcut, Siri and the Live
// Activity's button (docs/action-button.md). It is compiled into the app and the widget extension, as Apple requires
// for a control's intent.
//
// It is an AudioRecordingIntent, so it may start the microphone without opening the app, and a LiveActivityIntent, so
// the system runs it in the app's process (where the call and the microphone live) and lets it start the Live
// Activity that an AudioRecordingIntent must keep up while it records.
//
// The Action Button runs it once, after the hold, and never reports the release. So the first press opens the
// microphone and the turn ends itself after a pause; a press while the microphone is open sends; a press while eve
// speaks interrupts. While eve takes notes on the phone (owner, 2026-10-04: "the phone should hold the port until i
// click action button to turn it off"), a press stops the notes instead (PressAction.stopNotes).
import ActivityKit
import AppIntents

struct TalkIntent: AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource { "Talk to eve" }
    static var description: IntentDescription {
        IntentDescription("Opens the microphone and talks to eve. Press again to send, or just stop talking. While eve takes notes, a press stops them.")
    }
    /// The pairing is readable after the first unlock, so a locked phone can still call eve.
    static var authenticationPolicy: IntentAuthenticationPolicy { .alwaysAllowed }
    /// Background first; the app comes forward only when iOS will not start the Live Activity from the background.
    @available(iOS 26.0, *)
    static var supportedModes: IntentModes { [.background, .foreground(.dynamic)] }

    init() {}

    @MainActor
    func perform() async throws -> some IntentResult {
        #if WIDGET_EXTENSION
        // LiveActivityIntent: the system performs this in the app's process, never here.
        return .result()
        #else
        diag("intent: perform began")
        do {
            try await PhoneLink.press()
        } catch is TalkNeedsForeground {
            guard #available(iOS 26.0, *) else {
                throw TalkFailure(message: "Open Eve Link and hold to talk: iOS would not show the call panel.")
            }
            diag("intent: no Live Activity in the background; continuing in the foreground")
            try await continueInForeground(nil, alwaysConfirm: false)
            diag("intent: in the foreground")
            let relay = PhoneLink.relay
            if relay.notes.active ? TalkActivity.shared.ensureUp(.notes, since: relay.notesStarted)
                : TalkActivity.shared.ensureUp() {
                try await PhoneLink.press()
            } else {
                // Live Activities are off for Eve Link (Settings > Eve Link). An AudioRecordingIntent may not
                // record without one, so the intent ends first and the app, now in front, starts the turn itself.
                diag("intent: Live Activities are off; the app starts the turn after the intent")
                PhoneLink.pressAfterIntent()
            }
        } catch {
            diag("intent: failed: \(error.localizedDescription)")
            throw error
        }
        diag("intent: perform returning")
        return .result()
        #endif
    }
}

/// The Live Activity's End button: hang up the phone's call. Runs in the app's process, like TalkIntent.
struct EndCallIntent: LiveActivityIntent {
    static var title: LocalizedStringResource { "End the call with eve" }
    static var isDiscoverable: Bool { false }
    init() {}

    @MainActor
    func perform() async throws -> some IntentResult {
        #if !WIDGET_EXTENSION
        diag("intent: end call")
        PhoneLink.relay.endPhoneCall()
        #endif
        return .result()
    }
}

/// The Live Activity a phone call shows on the Lock Screen and in the Dynamic Island.
struct TalkActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable, Sendable {
        var phase: CallPhase
        var since: Date = .now          // when this phase began: the listening timer counts from it
        var heard: String = ""          // what eve heard last, one line
    }
}
