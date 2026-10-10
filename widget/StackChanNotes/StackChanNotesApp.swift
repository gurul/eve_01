// StackChan Notes helper — a Dock-less (LSUIElement) menu-bar app that mirrors
// the bridge daemon's diary files into the App Group container the widget
// reads, and hosts the windows the widget opens: the chat (stackchan://chat)
// and the diary (stackchan://diary).
// It is deliberately NOT sandboxed so it can read ~/.config/eve-bridge/notes.

import AppKit
import ServiceManagement
import SwiftUI
import WebKit
import os

@main
struct StackChanNotesApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    /// Window scenes can be built before the delegate runs, so register the brand
    /// fonts here too. Registration runs once however often it is called.
    init() { BrandFonts.register() }

    var body: some Scene {
        MenuBarExtra("StackChan Notes", systemImage: "note.text") {
            MenuContent()
        }
        // The diary window. `handlesExternalEvents` routes stackchan://diary here
        // (the URL scheme is declared in Info.plist through project.yml).
        Window("eve's diary", id: "diary") {
            DiaryView()
        }
        .windowResizability(.contentMinSize)
        .defaultSize(width: 720, height: 560)
        .handlesExternalEvents(matching: ["diary"])
        // The chat (ChatView.swift): eve's conversation, like Telegram, with a reply box.
        Window("eve chat", id: "chat") {
            ChatView()
        }
        .windowResizability(.contentMinSize)
        .defaultSize(width: 520, height: 760)
        .handlesExternalEvents(matching: ["chat"])
        Window("Eve Learning", id: "learning") {
            LearningDashboardView()
        }
        .defaultSize(width: 1200, height: 820)
        .handlesExternalEvents(matching: ["learning"])
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private let log = Logger(subsystem: "com.github.eve-bridge.StackChanNotes", category: "app")
    private static let registeredKey = "loginItemRegistered"

    func applicationDidFinishLaunching(_ notification: Notification) {
        BrandFonts.register()
        registerLoginItemOnce()
        NotesMirror.shared.start()
        PowerRelay.shared.start()
    }

    /// A URL open must bring the (LSUIElement) app forward so the window shows.
    func application(_ application: NSApplication, open urls: [URL]) {
        if urls.contains(where: { $0.scheme == "stackchan" }) {
            NSApp.activate(ignoringOtherApps: true)
            log.info("opening a window from a URL")
        }
    }

    /// Registers as a login item on the first launch only. Set EVE_NO_LOGIN_ITEM=1
    /// (e.g. for a smoke test from a build directory) to skip.
    private func registerLoginItemOnce() {
        let defaults = UserDefaults.standard
        guard !defaults.bool(forKey: Self.registeredKey) else { return }
        guard ProcessInfo.processInfo.environment["EVE_NO_LOGIN_ITEM"] == nil else {
            log.info("login item registration skipped (EVE_NO_LOGIN_ITEM)")
            return
        }
        do {
            try SMAppService.mainApp.register()
            defaults.set(true, forKey: Self.registeredKey)
            log.info("registered as login item")
        } catch {
            log.error("login item registration failed: \(error.localizedDescription, privacy: .public)")
        }
    }
}

private struct MenuContent: View {
    @Environment(\.openWindow) private var openWindow
    @State private var mirror = NotesMirror.shared
    @State private var launchAtLogin = SMAppService.mainApp.status == .enabled
    /// The daemon's answer to `mic status`; nil until asked, or when it is not reachable.
    @State private var mic: MicState?
    @State private var micAsked = false
    @State private var micOn = true
    /// eve's power switch, shared with the desktop card (PowerRelay).
    @State private var power = PowerRelay.shared

    var body: some View {
        Text(status)
        if let mood = mirror.snapshot.mood {
            Text("feeling \(MoodColor.emoji(for: mood.label)) \(mood.label)")
        }
        Text(mirror.notesDir.path)
            .font(.caption)
        if let err = mirror.lastError {
            Text("Error: \(err)")
        }
        Divider()
        // The owner's microphone switch: the same thing as `eve-bridge mic on|off`.
        // The mic itself opens only while the robot is connected (or EVE_MIC_ALWAYS=1);
        // this switch closes it for good until it is turned back on, across restarts.
        Text(micLine)
        Toggle("Microphone", isOn: $micOn)
            .disabled(mic == nil || !(mic?.available ?? false))
            .onChange(of: micOn) { _, on in
                guard let current = mic, current.switchOn != on else { return }
                Task { await flipMic(on) }
            }
        Divider()
        // The power switch: eve is the bridge daemon, a launchd agent that
        // restarts itself when killed. Off takes it out of launchd and keeps it
        // out across logins; on brings it back. See EveService.swift.
        Text(serviceLine)
            // A menu's `.task` runs once, not on every open, so keep both lines
            // fresh: launchd and the daemon are asked again every few seconds
            // while the view lives (the mic line too: a daemon just turned on
            // opens its socket a beat after it has a pid).
            .task {
                while !Task.isCancelled {
                    if !power.switching {
                        await power.refresh()
                        await refreshMic()
                    }
                    try? await Task.sleep(for: .seconds(5))
                }
            }
        if let title = power.state?.buttonTitle {
            Button(title) { Task { await flipService() } }
                .disabled(power.switching)
        }
        Divider()
        Button("Open chat") {
            NSApp.activate(ignoringOtherApps: true)
            openWindow(id: "chat")
        }
        .keyboardShortcut("t")
        Button("Open diary") {
            NSApp.activate(ignoringOtherApps: true)
            openWindow(id: "diary")
        }
        .keyboardShortcut("d")
        Button("Open learning dashboard") {
            NSApp.activate(ignoringOtherApps: true)
            openWindow(id: "learning")
        }
        Button("Refresh now") { mirror.sync() }
        Button("Open notes folder") { NSWorkspace.shared.open(mirror.notesDir) }
        Toggle("Launch at login", isOn: $launchAtLogin)
            .onChange(of: launchAtLogin) { _, on in
                do {
                    if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
                } catch {
                    launchAtLogin = SMAppService.mainApp.status == .enabled
                }
            }
        Divider()
        Button("Quit StackChan Notes") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
    }

    private var status: String {
        let when = mirror.lastSync.map { $0.formatted(date: .omitted, time: .shortened) } ?? "never"
        return "\(mirror.count) thoughts · synced \(when)"
    }

    private var micLine: String {
        if let mic { return mic.line }
        if let service = power.state, service.installed, !service.on, !service.reachable {
            return "Microphone: closed (eve is off)"
        }
        return micAsked ? "Microphone: daemon not reachable" : "Microphone: asking the daemon…"
    }

    private var serviceLine: String {
        if power.switching { return power.state?.on == true ? "eve: stopping…" : "eve: starting…" }
        return power.state?.line ?? "eve: asking launchd…"
    }

    /// Off then on is the same button: it reads where eve is and goes the other way.
    private func flipService() async {
        guard let current = power.state, !power.switching else { return }
        await power.turn(on: !current.on)
        // The mic line follows the daemon: gone when eve is off, back when it is on.
        await refreshMic()
    }

    private func refreshMic() async {
        let state = await EveDaemon.micStatus()
        mic = state
        micAsked = true
        if let state { micOn = state.switchOn }
    }

    private func flipMic(_ on: Bool) async {
        let state = await EveDaemon.setMic(on: on)
        mic = state
        micAsked = true
        // The daemon is the truth: if it could not be reached, show the switch where it really is.
        if let state { micOn = state.switchOn } else if let current = mic { micOn = current.switchOn }
    }
}


/// The same local web workspace as Windows, embedded in the widget's helper app.
struct LearningDashboardView: View {
    @State private var retry = UUID()
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                InkHeading("Eve Learning", size: 20)
                Spacer()
                Text("Start the bridge daemon if the workspace is offline.")
                    .font(BrandFont.body(12))
                    .foregroundStyle(Color.brandInkSoft)
                Button("Reload") { retry = UUID() }
                    .buttonStyle(ChunkyButtonStyle())
            }
            .padding(12)
            .background(Color.brandPaper)
            .overlay(alignment: .bottom) { Rectangle().fill(Color.brandInk).frame(height: 2) }
            .environment(\.colorScheme, .light)
            LearningWebView().id(retry)
        }
        .frame(minWidth: 850, minHeight: 600)
    }
}

struct LearningWebView: NSViewRepresentable {
    func makeNSView(context: Context) -> WKWebView {
        let view = WKWebView()
        let port = Int(ProcessInfo.processInfo.environment["EVE_LEARNING_PORT"] ?? "48766") ?? 48766
        view.load(URLRequest(url: URL(string: "http://127.0.0.1:\(port)/")!))
        return view
    }
    func updateNSView(_ view: WKWebView, context: Context) {}
}
