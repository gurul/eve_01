// Voice mode's host (docs/voice-mode.md): the top bar's voice button covers the screen with VoiceModeView. It is hold to
// talk: nothing listens until a finger holds the orb, and letting go sends (the Action Button's call path, Relay). This
// view owns the meter, turns the call's state into the line under the orb, and holds the two sheets voice mode opens.
// The X ends the call.
import SwiftUI

struct VoiceCallScreen: View {
    @ObservedObject var relay: Relay
    @ObservedObject var chat: ChatModel
    @ObservedObject var work: WorkModel
    @ObservedObject var log: Diag
    @Binding var talkError: String?
    let onAskText: () -> Void
    let onClose: () -> Void

    @State private var levels = VoiceLevels()
    @State private var showSettings = false
    @State private var showVoiceSettings = false
    /// Why the call could not start here (not paired, no microphone); the call's own errors come from Relay.
    @State private var startError: String?
    @AppStorage("voice.earpiece") private var earpiece = false
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        VoiceModeView(levels: levels, grade: grade, muted: relay.muted,
                      onMenu: { showSettings = true },
                      onSettings: { showVoiceSettings = true },
                      onAskText: onAskText,
                      onMute: { relay.muted.toggle() },
                      onClose: onClose)
            // Talking is the Action Button (owner, 2026-10-09: "hold to talk on the app should also be action not
            // screen hold"): it runs TalkIntent, which presses this same Relay, so the face and captions follow. A tap
            // on the face stops eve while he thinks or speaks (or stops notes); it never opens the microphone.
            .overlay {
                Rectangle()
                    .fill(.clear)
                    .frame(width: VoiceModeView.talkArea.width, height: VoiceModeView.talkArea.height)
                    .contentShape(Rectangle())
                    .offset(y: VoiceModeView.faceOffset)
                    .onTapGesture {
                        if relay.notes.active || relay.phase == .thinking || relay.phase == .speaking {
                            relay.endPhoneCall()
                        }
                    }
                    .accessibilityLabel("eve")
                    .accessibilityHint("Press the Action Button to talk. Tap to stop eve.")
                    .accessibilityAddTraits(.isButton)
            }
            .onChange(of: status, initial: true) { _, now in levels.status = now }
            .onChange(of: earpiece) { _, now in relay.useEarpiece(now) }
            .task { await start() }
            .onDisappear {
                relay.voiceLevels = nil
                relay.useEarpiece(false)
                if relay.phase != .ended { relay.endPhoneCall() }   // the call never outlives the screen
            }
            .sheet(isPresented: $showSettings) {
                SettingsSheet(relay: relay, log: log, work: work, talkError: $talkError)
            }
            .sheet(isPresented: $showVoiceSettings) {
                VoiceSettingsSheet(earpiece: $earpiece)
            }
    }

    /// The speed dial: the same stored choice as the chat's.
    private var grade: Binding<String> {
        Binding(get: { chat.grade.rawValue }, set: { chat.grade = ChatGrade.stored($0) })
    }

    private var status: String {
        let error = startError ?? relay.callError
        // Nobody is talking and eve is not answering: say how to talk.
        if (error ?? "").isEmpty, !relay.notes.active, relay.phase == .ended || relay.phase == .waiting {
            return VoiceStatus.holdToTalk
        }
        return VoiceStatus.text(phase: relay.phase, connected: relay.connected, notes: relay.notes, error: error)
    }

    private func start() async {
        relay.voiceLevels = levels
        relay.useEarpiece(earpiece)
        guard relay.pairing != nil else {
            startError = PhoneTurnError.notPaired.description      // before any microphone prompt
            return
        }
        // Ask for the microphone now, so the first hold talks instead of showing the prompt.
        if relay.phone?.micAllowed == nil { _ = await relay.phone?.requestMic() }
        startError = nil
    }
}

/// Voice settings: a short list. Today, where eve's voice plays.
struct VoiceSettingsSheet: View {
    @Binding var earpiece: Bool
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Picker("Play eve on", selection: $earpiece) {
                        Label("Speaker", systemImage: "speaker.wave.2").tag(false)
                        Label("Earpiece", systemImage: "ear").tag(true)
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                } header: {
                    Text("Sound")
                } footer: {
                    Text("Headphones and AirPods play eve whenever they are connected.")
                }
            }
            .navigationTitle("Voice")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
        .sensoryFeedback(.selection, trigger: earpiece)
    }
}
