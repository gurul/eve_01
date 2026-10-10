// The app's one Relay, reachable from outside the view tree: the Talk intent runs in the app's process without a
// window, so it cannot reach a @StateObject. The phone's microphone and the Live Activity are wired here.
import ActivityKit
import AppIntents
import Foundation

@MainActor
enum PhoneLink {
    static let relay: Relay = {
        let relay = Relay()
        relay.phone = PhoneAudio()
        // Notes count from when eve started them, so a call back after a drop keeps the timer.
        relay.onPhase = { [weak relay] in
            TalkActivity.shared.show($0, since: $0 == .notes ? relay?.notesStarted : nil)
        }
        relay.onHeard = { TalkActivity.shared.heard($0) }
        TalkActivity.shared.endStrays()    // a panel left over from a process iOS killed mid-call
        diag("app process up; paired: \(relay.pairing != nil)")
        Task { await relay.refreshAddress() }   // the daemon may have restarted since the last call
        return relay
    }()

    /// The Talk intent's press. Throws the message the intent shows (not paired, microphone not allowed).
    static func press() async throws {
        diag("press: relay ready")
        // An AudioRecordingIntent must have its Live Activity up before it records; without one App Intents traps
        // and the app "quit unexpectedly" (2026-10-04, two crash logs). So the panel comes first, and when iOS will
        // not start it from the background the intent continues in the foreground instead (TalkIntent).
        if relay.pressWouldStart, !TalkActivity.shared.ensureUp() {
            throw TalkNeedsForeground()
        }
        // Notes on the phone: the microphone is recording and this press is an AudioRecordingIntent, so the panel
        // must be up for it too. The minute keeper cannot bring it back from the background (review, 2026-10-04).
        if relay.notes.active, !TalkActivity.shared.ensureUp(.notes, since: relay.notesStarted) {
            throw TalkNeedsForeground()
        }
        do {
            try relay.press()
            diag("press: done")
        } catch let error as PhoneTurnError {
            throw TalkFailure(message: error.description)
        }
    }
}

extension PhoneLink {
    /// A press the open app makes just after the intent returned (Live Activities off: see TalkIntent).
    static func pressAfterIntent() {
        relay.noteLiveActivitiesOff()
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(400))
            do {
                try relay.press()
            } catch {
                diag("press after intent: \(error.localizedDescription)")
            }
        }
    }
}

/// The Live Activity could not start from the background: TalkIntent brings the app forward and presses there.
struct TalkNeedsForeground: Error {}

/// An intent error whose text is shown as it is.
struct TalkFailure: Error, CustomLocalizedStringResourceConvertible {
    let message: String
    var localizedStringResource: LocalizedStringResource { "\(message)" }
}

/// Starts, updates and ends the call's Live Activity. Updates run one after another, off the main actor (ActivityKit's
/// update and end are @concurrent), so a quick listening → thinking → speaking lands in order.
@MainActor
final class TalkActivity {
    static let shared = TalkActivity()
    private var current: String?           // the live activity's id
    private var lastPhase: CallPhase = .ended
    private var since = Date.now
    private var lastHeard = ""

    /// What eve heard, shown under the state in the expanded island and on the Lock Screen.
    func heard(_ text: String) {
        lastHeard = String(text.prefix(140))
        if lastPhase != .ended { show(lastPhase) }
    }
    private var queue: Task<Void, Never>?

    /// Start the panel now if it is not up (listening). True when it is up. Tries even when
    /// `areActivitiesEnabled` says no: launched in the background it said no while the panel could start (logged).
    func ensureUp(_ phase: CallPhase = .listening, since given: Date? = nil) -> Bool {
        if Self.isLive(current) { return true }
        show(phase, since: given)
        return Self.isLive(current)
    }

    /// `since`: when the phase began, when it did not begin now (notes on the phone count from when eve started
    /// them). Called again with the same phase, it brings the panel back if it was swiped away: notes keep the
    /// microphone open, and an AudioRecordingIntent's recording must keep its Live Activity up.
    func show(_ phase: CallPhase, since given: Date? = nil) {
        if phase != lastPhase {
            since = given ?? .now
            if phase == .listening || phase == .notes { lastHeard = "" }   // a new turn: last turn's words go
        } else if let given {
            since = given
        }
        lastPhase = phase
        let state = TalkActivityAttributes.ContentState(phase: phase, since: since, heard: lastHeard)
        let content = ActivityContent(state: state, staleDate: nil)
        if phase != .ended, !Self.isLive(current) {
            // None yet, or the owner swiped it away. Requested synchronously: an AudioRecordingIntent needs the Live
            // Activity up before it records. Any other Eve Link activity is a stray: end it, so one panel shows.
            current = nil
            if !ActivityAuthorizationInfo().areActivitiesEnabled {
                diag("live activity: areActivitiesEnabled is false; trying anyway")
            }
            let activity: Activity<TalkActivityAttributes>
            do {
                activity = try Activity.request(attributes: TalkActivityAttributes(), content: content, pushType: nil)
            } catch {
                diag("live activity: request failed: \(error.localizedDescription)")
                return
            }
            diag("live activity: up (\(phase.rawValue))")
            current = activity.id
            endStrays()
            return
        }
        guard let id = current else { return }
        if phase == .ended { current = nil }
        let previous = queue
        queue = Task.detached {
            await previous?.value
            guard let activity = Activity<TalkActivityAttributes>.activities.first(where: { $0.id == id }) else { return }
            if phase == .ended {
                await activity.end(content, dismissalPolicy: .immediate)
            } else {
                await activity.update(content)
            }
        }
    }

    /// End every Eve Link activity but the current one. The ids are taken now, so an activity requested after this
    /// call is never ended by it.
    func endStrays() {
        let strays = Activity<TalkActivityAttributes>.activities.map(\.id).filter { $0 != current }
        guard !strays.isEmpty else { return }
        let previous = queue
        queue = Task.detached {
            await previous?.value
            for activity in Activity<TalkActivityAttributes>.activities where strays.contains(activity.id) {
                await activity.end(nil, dismissalPolicy: .immediate)
            }
        }
    }

    private static func isLive(_ id: String?) -> Bool {
        guard let id else { return false }
        return Activity<TalkActivityAttributes>.activities.contains {
            $0.id == id && $0.activityState != .ended && $0.activityState != .dismissed
        }
    }
}

/// Settings > Action Button > Shortcut, Spotlight and Siri.
struct EveLinkShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: TalkIntent(),
                    phrases: ["Talk to \(.applicationName)", "Talk to eve with \(.applicationName)",
                              "Call eve with \(.applicationName)"],
                    shortTitle: "Talk to eve",
                    systemImageName: "waveform")
    }
}
