// Settings (the menu button, docs/app-chat.md): what the old main screen had except Hold to talk, which is Talk now.
// The phone's call state, eve's link, what eve remembers (Memory, moved here from Apps on 2026-10-10: "move
// memories to settings"), pairing and the diagnostics log.
import SwiftUI
import UIKit

struct SettingsSheet: View {
    @ObservedObject var relay: Relay
    @ObservedObject var log: Diag
    @ObservedObject var work: WorkModel
    @Binding var talkError: String?
    @State private var pasted = ""
    @State private var pasteFailed = false
    @State private var copied = false
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    var body: some View {
        NavigationStack {
            List {
                if work.needsYou > 0 {
                    Section {
                        Label(work.needsYou == 1 ? "A job is waiting on your Go. It's in Work."
                              : "\(work.needsYou) jobs are waiting on you. They're in Work.",
                              systemImage: "circle.fill")
                            .labelStyle(DotLabelStyle())
                    }
                }
                Section {
                    row("Phone", relay.notes.active ? "Taking notes" : relay.phase == .ended ? "Idle" : relay.phase.title)
                    if let talkError { Text(talkError).foregroundStyle(.red) }
                    if relay.liveActivitiesOff {
                        Text("Live Activities are off for Eve Link, so the Action Button opens the app to talk. Turn them on in Settings > Eve Link > Live Activities to talk from the Lock Screen.")
                            .font(.footnote).foregroundStyle(.orange)
                    }
                    if relay.phase != .ended {
                        Button("End call", role: .destructive) { relay.endPhoneCall() }
                    }
                } header: {
                    Text("Talk on this phone")
                } footer: {
                    Text("The Action Button can't see a release: assign Talk to eve to it, hold it, talk, and eve sends when you pause, or press again to send.")
                }
                Section("eve") {
                    row("Paired with", relay.pairing?.base.host ?? "not paired")
                    row("Call", relay.callStatus)
                    if !relay.lastHeard.isEmpty { row("Heard", relay.lastHeard) }
                }
                if work.controls?.memory == true {
                    Section {
                        NavigationLink {
                            MemoryControls(client: work.client)
                                .background(Color.eveBackground)
                                .navigationTitle("Memory")
                                .navigationBarTitleDisplayMode(.inline)
                        } label: {
                            Label("Memory", systemImage: ControlKind.memory.symbol)
                        }
                    } footer: {
                        Text("What eve remembers about you: search it, add to it, hide a line or forget it.")
                    }
                }
                if let token = relay.pairing?.token {
                    let topic = NotifyTopic.topic(token: token)
                    Section {
                        Text(topic).font(.footnote.monospaced()).textSelection(.enabled)
                        Button(copied ? "Copied" : "Copy") {
                            UIPasteboard.general.string = topic
                            copied = true
                        }
                        Button("Open ntfy") {
                            openURL(NotifyTopic.ntfyApp) { opened in
                                if !opened { openURL(NotifyTopic.appStore) }
                            }
                        }
                    } header: {
                        Text("Notifications")
                    } footer: {
                        Text("In ntfy, tap +, paste this topic and keep the default server. Anyone with the topic can read eve's notifications; it comes from your link token.")
                    }
                }
                Section {
                    TextField("evelink://pair?…", text: $pasted)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityLabel("Pairing link")
                    Button("Pair from a pasted link") {
                        if let url = URL(string: pasted.trimmingCharacters(in: .whitespaces)) {
                            pasteFailed = !relay.pair(with: url)
                            if !pasteFailed { pasted = "" }
                        } else {
                            pasteFailed = true
                        }
                    }
                    if pasteFailed { Text("That isn't an Eve Link pairing link.").foregroundStyle(.red) }
                } header: {
                    Text("Pairing")
                } footer: {
                    Text("Send /stick to eve in Telegram and tap Update Eve Link. Do the same after eve restarts: the pinned message's button has the new address.")
                }
                Section {
                    if log.lines.isEmpty {
                        Text("Nothing yet. Press the Action Button or talk.").foregroundStyle(.secondary)
                    }
                    ForEach(Array(log.lines.reversed().enumerated()), id: \.offset) { _, line in
                        Text(line).font(.caption.monospaced()).textSelection(.enabled)
                    }
                    if !log.lines.isEmpty {
                        Button("Copy log") { UIPasteboard.general.string = log.lines.joined(separator: "\n") }
                        Button("Clear log", role: .destructive) { log.clear() }
                    }
                } header: {
                    Text("Diagnostics")
                }
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
        .presentationDragIndicator(.visible)
        .task { if work.controls == nil { await work.refreshControls() } }
    }

    private func row(_ label: String, _ value: String) -> some View {
        LabeledContent(label) { Text(value).multilineTextAlignment(.trailing) }
    }
}

/// A small blue dot before the words, as on the menu button.
private struct DotLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 10) {
            Circle().fill(Color.blue).frame(width: 8, height: 8).accessibilityHidden(true)
            configuration.title
        }
    }
}
