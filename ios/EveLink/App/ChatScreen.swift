// Eve Link's one screen (docs/app-chat.md): a top bar (Settings, the Chat | Work pill, Talk) over either the chat or
// the Work lists. The chat is eve's messages, drawn by OpenUI in the bundled web view, above a native composer.
// Typing goes to the page with window.eveNative.send, so a typed turn looks exactly as one typed in OpenUI.
import PhotosUI
import SwiftUI
import UIKit

enum HomeMode: String, CaseIterable {
    case chat, apps, work
    var title: String { rawValue.capitalized }
}

struct HomeScreen: View {
    @ObservedObject var relay: Relay
    @ObservedObject var chat: ChatModel
    @ObservedObject var work: WorkModel
    @ObservedObject var log: Diag
    @Binding var talkError: String?
    @AppStorage("home.mode") private var mode: HomeMode = .chat
    @State private var showSettings = false
    @State private var showVoice = false
    /// Bumped to put the cursor in the composer (voice mode's "Ask eve").
    @State private var composerFocus = 0
    #if DEBUG && targetEnvironment(simulator)
    /// Bumped to type "/" in the composer (the design hook's -eveSlash).
    @State private var composerSlash = 0
    #endif
    /// Where the native chrome covers the web view, in global points: the page pads its list by them (setInsets).
    @State private var webFrame = CGRect.zero
    @State private var barBottom: CGFloat = 0
    @State private var composerTop: CGFloat = 0
    /// The composer's dictation, held here so Talk, a call and the background can stop it.
    @StateObject private var dictation = Dictation()
    @Environment(\.scenePhase) private var scenePhase

    private var chatShown: Bool { relay.pairing != nil && mode == .chat }

    var body: some View {
        ZStack(alignment: .top) {
            Color.eveBackground.ignoresSafeArea()
            if relay.pairing != nil {
                // The whole screen, keyboard included, under the top bar and the composer: the page pads its list
                // by what covers it (window.eveNative.setInsets), so nothing sits under the chrome at rest.
                // Measured inside ignoresSafeArea: outside it the reader sees the safe-area frame, not the view's.
                // Kept alive on Apps and Work, only hidden: tearing it down reloaded the page on every tab switch
                // (owner, 2026-10-08: "the switched between tabs are jittery").
                ChatWebView(model: chat)
                    .onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: {
                        webFrame = $0
                        sendInsets()
                    }
                    .ignoresSafeArea()
                    .opacity(chatShown ? 1 : 0)
                    .allowsHitTesting(chatShown)
                    .accessibilityHidden(!chatShown)
            }
            VStack(spacing: 0) {
                TopBar(mode: $mode, needsYou: work.needsYou > 0, callLive: relay.phase != .ended || relay.notes.active,
                       settings: { showSettings = true },
                       talk: {
                           dictation.stop(releaseSession: relay.phase == .ended)   // voice mode is about to take the mic
                           showVoice = true
                       })
                    .background(alignment: .top) { if chatShown { TopFade().padding(.bottom, -TopFade.overhang) } }
                    .onGeometryChange(for: CGFloat.self) { $0.frame(in: .global).maxY } action: {
                        barBottom = $0
                        sendInsets()
                    }
                if relay.pairing == nil {
                    PairPrompt(relay: relay)
                } else if mode == .chat {
                    Spacer(minLength: 0)
                    ChatComposer(chat: chat, relay: relay, dictation: dictation, commands: work.commands,
                                 focusRequest: composerFocus, slashRequest: slashRequest)
                        .background(alignment: .top) { ComposerFloor() }
                        .onGeometryChange(for: CGFloat.self) { $0.frame(in: .global).minY } action: {
                            composerTop = $0
                            sendInsets()
                        }
                } else {
                    WorkView(work: work, shows: mode)
                }
            }
        }
        // A call (voice mode, the Action Button) stops dictation before it starts the mic, and leaves the session alone.
        .onAppear {
            let mic = dictation
            relay.beforeMic = { [weak mic] in mic?.stop(releaseSession: false) }
        }
        // The app keeps audio in the background for calls; dictation must not keep the mic on there.
        .onChange(of: scenePhase) { _, now in if now == .background { dictation.stop() } }
        .sheet(isPresented: $showSettings) {
            SettingsSheet(relay: relay, log: log, work: work, talkError: $talkError)
        }
        .fullScreenCover(isPresented: $showVoice) {
            VoiceCallScreen(relay: relay, chat: chat, work: work, log: log, talkError: $talkError,
                            onAskText: {
                                closeVoice()
                                mode = .chat
                                Task {
                                    try? await Task.sleep(for: .milliseconds(450))   // after the cover slides away
                                    composerFocus += 1
                                }
                            },
                            onClose: closeVoice)
        }
        // The menu button's dot: jobs waiting on the owner, read every 60 s while the app is open and paired.
        .task(id: scenePhase == .active && relay.pairing != nil) {
            guard scenePhase == .active, relay.pairing != nil else { return }
            await work.refreshControls()      // Apps opens on data, not "Loading…"
            while !Task.isCancelled {
                await work.refreshJobs()
                try? await Task.sleep(for: .seconds(60))
            }
        }
        // The composer's / menu: read when the chat first shows and again for a new pairing (another Mac's eve).
        .task(id: relay.pairing) {
            guard relay.pairing != nil else { return }
            await work.refreshCommands()
        }
        #if DEBUG && targetEnvironment(simulator)
        .task { openDesignScreen() }
        #endif
    }

    private var slashRequest: Int {
        #if DEBUG && targetEnvironment(simulator)
        composerSlash
        #else
        0
        #endif
    }

    /// The top bar's bottom and the composer's top, as the web view's own top and bottom insets (CSS px are points).
    private func sendInsets() {
        guard chatShown, webFrame.height > 0, barBottom > 0, composerTop > 0 else { return }
        chat.setInsets(top: barBottom - webFrame.minY + TopFade.overhang, bottom: webFrame.maxY - composerTop)
    }

    /// Voice mode closes (its X, or Ask eve): the call ends with it.
    private func closeVoice() {
        relay.endHandsFree()
        showVoice = false
    }

    #if DEBUG && targetEnvironment(simulator)
    /// The simulator's design hook (docs/app-chat.md): -eveScreen chat | work | voice | settings, -eveGrade,
    /// -eveKeyboard YES (chat with the keyboard up), -eveSlash YES (chat with "/" typed: the / menu).
    private func openDesignScreen() {
        if let grade = DesignHook.grade.flatMap(ChatGrade.init(rawValue:)) { chat.grade = grade }
        if let phase = DesignHook.activity.flatMap(CallPhase.init(rawValue:)) {
            TalkActivity.shared.show(phase, since: .now.addingTimeInterval(-7))
        }
        switch DesignHook.screen ?? (DesignHook.slash ? "chat" : nil) {
        case "chat":
            mode = .chat
            if DesignHook.keyboard || DesignHook.slash {
                Task {
                    try? await Task.sleep(for: .seconds(2))     // the page says ready first
                    if DesignHook.slash { composerSlash += 1 } else { composerFocus += 1 }
                }
            }
        case "work": mode = .work
        case "apps": mode = .apps
        case "voice": showVoice = true
        case "settings": showSettings = true
        default: break
        }
    }
    #endif
}

/// Under the top bar while the chat shows: the screen's color behind the status bar, fading to clear at the bar's
/// bottom, so a message scrolled up fades out and never meets the clock. It ends where the page's list begins.
private struct TopFade: View {
    /// How far the fade runs below the bar: text scrolling up dissolves here, before it reaches the buttons. The
    /// page's top padding grows by the same amount, so at rest the first message starts below it.
    static let overhang: CGFloat = 28

    var body: some View {
        LinearGradient(stops: [.init(color: .eveBackground, location: 0),
                               .init(color: .eveBackground, location: 0.8),        // solid to the bar's bottom edge
                               .init(color: .eveBackground.opacity(0.7), location: 0.9),
                               .init(color: .eveBackground.opacity(0), location: 1)],
                       startPoint: .top, endPoint: .bottom)
            .ignoresSafeArea(edges: .top)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}

/// Behind the composer: clear at its top edge, the screen's color by 18 pt down and to the bottom of the screen, so the
/// list never shows around or under the card. It starts at the card's top: the last message (16 pt above) is clear.
private struct ComposerFloor: View {
    var body: some View {
        VStack(spacing: 0) {
            LinearGradient(colors: [.eveBackground.opacity(0), .eveBackground], startPoint: .top, endPoint: .bottom)
                .frame(height: 18)
            Color.eveBackground
        }
        .ignoresSafeArea(edges: .bottom)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// Settings at left (a blue dot when a job waits on the owner), the Chat | Apps | Work pill, Talk at right.
private struct TopBar: View {
    @Binding var mode: HomeMode
    let needsYou: Bool
    let callLive: Bool
    let settings: () -> Void
    let talk: () -> Void
    @Namespace private var pill

    var body: some View {
        HStack {
            Button(action: settings) {
                MenuGlyph()
                    .overlay(alignment: .topTrailing) {
                        if needsYou {
                            Circle().fill(Color.blue).frame(width: 9, height: 9).offset(x: 2, y: -4)
                        }
                    }
            }
            .buttonStyle(RoundButtonStyle())
            .foregroundStyle(.primary)
            .accessibilityLabel("Settings")
            .accessibilityValue(needsYou ? "A job is waiting on you" : "")

            Spacer(minLength: 8)
            HStack(spacing: 0) {
                ForEach(HomeMode.allCases, id: \.self) { m in
                    Button {
                        mode = m                          // the content swaps at once; only the pill's highlight moves
                    } label: {
                        Text(m.title)
                            .font(.body)
                            .foregroundStyle(.primary)
                            .frame(minWidth: 64, minHeight: 40)
                            .padding(.horizontal, 4)
                            .background {
                                if mode == m {
                                    Capsule().fill(Color.eveSelected).matchedGeometryEffect(id: "segment", in: pill)
                                }
                            }
                            .contentShape(Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(mode == m ? .isSelected : [])
                }
            }
            .animation(.snappy(duration: 0.25), value: mode)
            .padding(4)
            .background(Capsule().fill(Color.eveFill))
            .overlay(Capsule().strokeBorder(Color.eveEdge, lineWidth: 1))
            .sensoryFeedback(.selection, trigger: mode)
            Spacer(minLength: 8)

            Button(action: talk) {
                Image(systemName: "waveform")
                    .font(.body.weight(.semibold))
                    .foregroundStyle(callLive ? Color.red : Color.primary)
                    .symbolEffect(.variableColor.iterative, isActive: callLive)
            }
            .buttonStyle(RoundButtonStyle())
            .accessibilityLabel("Voice")
            .accessibilityValue(callLive ? "On a call with eve" : "")
            .accessibilityHint("Opens voice mode: talk with eve hands-free")
        }
        .padding(.horizontal, 16)
        .padding(.top, 6)
        .padding(.bottom, 8)
    }
}

/// Unpaired: say how to pair, and take a copied pairing link. Never an empty web view.
private struct PairPrompt: View {
    @ObservedObject var relay: Relay
    @State private var failed = false

    var body: some View {
        ContentUnavailableView {
            Label("Pair with eve", systemImage: "link")
        } description: {
            Text("On the Mac, run eve-bridge link pair and open the link on this phone. Or copy the pairing link and paste it here.")
        } actions: {
            Button("Paste pairing link") {
                let text = UIPasteboard.general.string?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                failed = !(URL(string: text).map { relay.pair(with: $0) } ?? false)
            }
            .buttonStyle(.borderedProminent)
            if failed {
                Text("That isn't an Eve Link pairing link.").font(.footnote).foregroundStyle(.red)
            }
        }
        .frame(maxHeight: .infinity)
    }
}

/// One rounded card: the photos and files picked (if any), the field, and below them "+" (attach), the speed dial, the mic
/// (dictation) and send, which turns into stop while eve answers. Sending uploads the attachments first.
struct ChatComposer: View {
    @ObservedObject var chat: ChatModel
    @ObservedObject var relay: Relay
    @ObservedObject var dictation: Dictation
    /// The / menu's words (WorkModel.commands): the daemon's list, or the built-in one.
    var commands: [SlashCommand] = SlashMenu.builtIn
    /// Bumped by the screen to put the cursor here (voice mode's "Ask eve").
    var focusRequest = 0
    /// Bumped by the screen to type "/" here and open the / menu (the simulator's design hook).
    var slashRequest = 0
    @State private var draft = ""
    @State private var beforeDictation = ""
    @State private var sent = 0
    @State private var attachments: [PendingAttachment] = []
    @State private var showPhotos = false
    @State private var showFiles = false
    @State private var picked: [PhotosPickerItem] = []
    @State private var uploading = false
    @State private var attachProblem: String?
    /// A row of the / menu was tapped: the selection haptic.
    @State private var picks = 0
    @FocusState private var focused: Bool

    private var trimmed: String { draft.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var tooLong: Bool { ChatBridgeMessage.tooLong(trimmed) }
    private var canSend: Bool { chat.ready && !uploading && (!trimmed.isEmpty || !attachments.isEmpty) && !tooLong }
    private var roomLeft: Int { ChatAttachments.maxCount - attachments.count }
    /// The / menu's rows for what is typed; nil or empty hides it. Not while attachments wait: a code word goes alone.
    private var slashRows: [SlashCommand]? {
        guard chat.ready, attachments.isEmpty, !uploading else { return nil }
        return SlashMenu.matches(commands, draft: draft).flatMap { $0.isEmpty ? nil : $0 }
    }

    var body: some View {
        VStack(spacing: 6) {
            if tooLong {
                note("Too long: \(ChatBridgeMessage.maxText) characters at most.", .red)
            } else if let attachProblem {
                note(attachProblem, .red)
            } else if let problem = dictation.problem {
                note(problem, .orange)
            }
            VStack(alignment: .leading, spacing: 10) {
                if !attachments.isEmpty {
                    AttachmentStrip(items: attachments) { id in
                        withAnimation(.snappy(duration: 0.2)) { attachments.removeAll { $0.id == id } }
                    }
                    .disabled(uploading)
                }
                TextField("Ask eve", text: $draft, axis: .vertical)
                    .font(.body)
                    .lineLimit(1...6)
                    .focused($focused)
                    .disabled(!chat.ready)
                    .padding(.horizontal, 4)
                    .accessibilityLabel("Message eve")
                HStack(spacing: 18) {
                    AttachButton(full: roomLeft <= 0 || uploading, ready: chat.ready,
                                 photos: { showPhotos = true }, files: { showFiles = true }, commands: openCommands)
                    Spacer()
                    SpeedDial(grade: $chat.grade)
                    micButton
                    sendButton
                }
            }
            .padding(.horizontal, 14)
            .padding(.top, 14)
            .padding(.bottom, 10)
            .background(RoundedRectangle(cornerRadius: 24, style: .continuous).fill(Color.eveFill))
            .overlay(RoundedRectangle(cornerRadius: 24, style: .continuous).strokeBorder(Color.eveEdge, lineWidth: 1))
            .contentShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
            .onTapGesture { if chat.ready { focused = true } }
            // Above the card, over the chat: an overlay, so the composer's height (and the page's bottom inset) does
            // not jump while the owner types a command.
            .overlay(alignment: .top) {
                if let rows = slashRows {
                    // its own height up from the card's top edge, 8 pt clear of it
                    SlashMenuList(rows: rows, pick: pickCommand)
                        .offset(y: -SlashMenuList.height(rows.count) - 8)
                        .transition(.opacity.combined(with: .offset(y: 6)))
                }
            }
            .animation(.snappy(duration: 0.18), value: slashRows == nil)
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
        .padding(.bottom, 8)
        .sensoryFeedback(.impact(weight: .light), trigger: sent)
        .sensoryFeedback(.selection, trigger: picks)
        // a call takes the mic: Relay.beforeMic stopped dictation already; never release the call's session here
        .onChange(of: relay.phase) { _, now in if now != .ended { dictation.stop(releaseSession: false) } }
        .onDisappear { dictation.stop() }
        .onChange(of: focusRequest) { if chat.ready { focused = true } }
        .onChange(of: slashRequest) { openCommands() }
        .photosPicker(isPresented: $showPhotos, selection: $picked, maxSelectionCount: max(roomLeft, 1),
                      selectionBehavior: .ordered, matching: .images, preferredItemEncoding: .compatible)
        .onChange(of: picked) { _, items in
            guard !items.isEmpty else { return }
            picked = []
            Task { await addPhotos(items) }
        }
        .fileImporter(isPresented: $showFiles, allowedContentTypes: AttachmentPrep.fileTypes,
                      allowsMultipleSelection: true) { result in
            switch result {
            case .success(let urls): Task { await addFiles(urls) }
            case .failure: attachProblem = "Couldn't open Files."
            }
        }
    }

    private func add(_ item: PendingAttachment) {
        guard attachments.count < ChatAttachments.maxCount else {
            attachProblem = "Four attachments at most."
            return
        }
        withAnimation(.snappy(duration: 0.2)) { attachments.append(item) }
    }

    private func addPhotos(_ items: [PhotosPickerItem]) async {
        attachProblem = nil
        for item in items.prefix(max(roomLeft, 0)) {
            guard let data = try? await item.loadTransferable(type: Data.self),
                  let photo = await AttachmentPrep.photo(data, name: "Photo") else {
                attachProblem = "Couldn't read that photo."
                continue
            }
            add(photo)
        }
    }

    private func addFiles(_ urls: [URL]) async {
        attachProblem = nil
        if urls.count > roomLeft { attachProblem = "Four attachments at most." }
        for url in urls.prefix(max(roomLeft, 0)) {
            switch await AttachmentPrep.file(url) {
            case .success(let item): add(item)
            case .failure(let e): attachProblem = e.message
            }
        }
    }

    private func note(_ text: String, _ color: Color) -> some View {
        Text(text).font(.footnote).foregroundStyle(color).lineLimit(2).padding(.horizontal, 8)
    }

    private var micButton: some View {
        Button(action: toggleDictation) {
            Image(systemName: dictation.listening ? "mic.fill" : "mic")
                .font(.system(size: 20, weight: .regular))
                .foregroundStyle(dictation.listening ? Color.red : Color.primary)
                .symbolEffect(.pulse, isActive: dictation.listening)
                .frame(width: 36, height: 36)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!chat.ready)
        .accessibilityLabel(dictation.listening ? "Stop dictation" : "Dictate")
        .accessibilityHint("Types what you say into the message. It does not send it.")
    }

    @ViewBuilder private var sendButton: some View {
        if uploading {
            ProgressView()
                .controlSize(.small)
                .frame(width: 36, height: 36)
                .accessibilityLabel("Sending attachments")
        } else if chat.busy {
            Button {
                dictation.stop()
                chat.stop()
            } label: {
                Image(systemName: "stop.fill")
                    .font(.system(size: 13, weight: .bold))
                    .foregroundStyle(Color(uiColor: .systemBackground))
                    .frame(width: 36, height: 36)
                    .background(Circle().fill(Color.primary))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Stop")
            .accessibilityHint("Stops showing eve's reply")
        } else {
            Button(action: send) {
                Image(systemName: "arrow.up")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(Color(uiColor: .systemBackground))
                    .frame(width: 36, height: 36)
                    .background(Circle().fill(Color.primary.opacity(canSend ? 1 : 0.3)))
            }
            .buttonStyle(.plain)
            .disabled(!canSend)
            .accessibilityLabel("Send")
        }
    }

    /// "Commands" in the "+" menu: "/" in the field, which opens the / menu, and the cursor after it.
    private func openCommands() {
        guard chat.ready else { return }
        attachProblem = nil
        if SlashMenu.query(draft) == nil { draft = "/" }
        focused = true
    }

    /// A tapped row: sent at once, the way the send button sends, and the field cleared.
    private func pickCommand(_ command: SlashCommand) {
        guard chat.ready else { return }
        dictation.stop()
        picks += 1
        chat.send(command.text)
        draft = ""
    }

    private func toggleDictation() {
        if dictation.listening {
            dictation.stop()
            return
        }
        if relay.phase != .ended || relay.notes.active {
            dictation.problem = "You're on a call with eve. End it to dictate."
            return
        }
        beforeDictation = draft
        Task {
            await dictation.start { said in draft = DictationText.join(beforeDictation, said) }
        }
    }

    private func send() {
        guard canSend else { return }
        dictation.stop()
        attachProblem = nil
        guard !attachments.isEmpty else {
            chat.send(trimmed)
            draft = ""
            sent += 1
            return
        }
        // Upload first, then the turn names them. A failed upload keeps everything here, with one line saying why.
        let text = trimmed, items = attachments
        uploading = true
        sent += 1
        Task {
            var done: [ChatUploaded] = []
            for item in items {
                switch await chat.upload(data: item.data, name: item.name, mime: item.mime) {
                case .success(var up):
                    up.thumb = item.thumbDataURL
                    done.append(up)
                case .failure(let f):
                    attachProblem = f.message
                    uploading = false
                    return
                }
            }
            chat.send(text, attachments: done)
            if draft.trimmingCharacters(in: .whitespacesAndNewlines) == text { draft = "" }
            attachments.removeAll { item in items.contains { $0.id == item.id } }
            uploading = false
        }
    }
}

/// Auto, Instant or Think, remembered; sent with each turn as its grade.
private struct SpeedDial: View {
    @Binding var grade: ChatGrade

    var body: some View {
        Menu {
            Picker("Speed", selection: $grade) {
                ForEach(ChatGrade.allCases, id: \.self) { g in
                    Text(g.title).accessibilityHint(g.detail)
                }
            }
            .pickerStyle(.inline)
        } label: {
            HStack(spacing: 4) {
                Image(systemName: "gauge.with.dots.needle.33percent")
                    .font(.system(size: 20, weight: .regular))
                if grade != .auto {
                    Text(grade.title).font(.subheadline.weight(.medium))
                }
            }
            .foregroundStyle(grade == .auto ? Color.primary : Color.blue)
            .frame(minWidth: 36, minHeight: 36)
            .contentShape(Rectangle())
        }
        .menuOrder(.fixed)
        .accessibilityLabel("Speed")
        .accessibilityValue(grade.title)
        .accessibilityHint("Auto, Instant or Think")
        .sensoryFeedback(.selection, trigger: grade)
    }
}
