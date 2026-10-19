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
    @ObservedObject private var location = LocationSharer.shared
    @State private var pasted = ""
    @State private var pasteFailed = false
    @State private var copied = false
    @State private var confirmForget = false
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
                locationSection
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
                    Text("On the Mac, run eve-bridge link pair and open the link on this phone. After eve restarts, the app finds its new address by itself.")
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

    /// Share location (docs/app-chat.md, "Location"): the switch, what iOS allows, when the Mac last heard.
    private var locationSection: some View {
        Section {
            Toggle("Share location", isOn: Binding(get: { location.enabled }, set: { location.setEnabled($0) }))
            row("Permission", location.permission.title + (location.approximate ? ", approximate" : ""))
            TimelineView(.periodic(from: .now, by: 30)) { context in
                row("Last sent", LocationAge.words(since: location.lastSent, now: context.date))
            }
            if location.enabled, let problem = location.problem {
                Text(problem).font(.footnote).foregroundStyle(.orange)
            }
            if location.enabled {
                switch location.permission {
                case .notAsked:
                    Button("Allow location") { location.askIfNeeded() }
                        .disabled(relay.pairing == nil)
                case .denied:
                    Text("Location is off for Eve Link. Turn it on in Settings > Eve Link > Location.")
                        .font(.footnote).foregroundStyle(.orange)
                    Button("Open Settings") { location.openSettings() }
                case .restricted:
                    Text("This phone doesn't allow location for apps (Screen Time or a profile).")
                        .font(.footnote).foregroundStyle(.orange)
                case .whileUsing:
                    Text("eve sees where you are only while Eve Link is open. Choose Always in Settings so it keeps up when the app is closed.")
                        .font(.footnote).foregroundStyle(.secondary)
                    Button("Open Settings") { location.openSettings() }
                case .always:
                    EmptyView()
                }
                if location.permission.granted, location.approximate {
                    Text("Precise Location is off, so eve only knows the area you're in. Turn it on in Settings > Eve Link > Location.")
                        .font(.footnote).foregroundStyle(.secondary)
                    Button("Open Settings") { location.openSettings() }
                }
            }
            // The Mac's places (bridge places.py): how many it knows, and a way to make it forget them all
            if let places = location.places, places.on {
                row("Places eve knows", places.words)
                Button("Forget location history", role: .destructive) { confirmForget = true }
                    .disabled(location.forgetting || relay.pairing == nil)
            }
            if let problem = location.placesProblem {
                Text(problem).font(.footnote).foregroundStyle(.secondary)
            }
        } header: {
            Text("Location")
        } footer: {
            Text("eve uses where your phone is to answer things like what's near you and how long to get home, and remembers the places you go so it can learn your routines. It all stays on your Mac.")
        }
        .task { await location.refreshPlaces() }
        .confirmationDialog("Forget location history?", isPresented: $confirmForget, titleVisibility: .visible) {
            Button("Forget everything", role: .destructive) { Task { await location.forgetPlaces() } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("eve forgets every place and visit, the things you asked it to remember where they are, and where your phone is now. This can't be undone.")
        }
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
