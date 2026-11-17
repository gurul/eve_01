// The chat (docs/app-chat.md): eve's one conversation, drawn natively. The message list is SwiftUI
// (Packages/EveChat), cards are drawn by json-render's Swift renderer (Packages/EveCards), and everything that
// reaches the Mac goes through ChatTransport with the link token. This file joins them to the app: the composer's
// sends and uploads, Stop, the eve/Muse switch, haptics, links, Quick Look, and what happens in the background.
import EveChatCore
import EveChatUI
import QuickLook
import SafariServices
import SwiftUI
import UIKit

/// The chat's engine and transport, kept for the app's life so switching to Work never reloads the conversation.
@MainActor
final class ChatModel: NSObject, ObservableObject {
    /// The app is paired and the chat has started: the composer may send. A send made before the history arrives
    /// waits for it.
    @Published private(set) var ready = false
    /// A turn from this app is streaming: the send button is a stop button while the composer has nothing to send.
    @Published private(set) var busy = false
    /// The composer's speed dial: sent with every turn as "grade", and remembered.
    @Published var grade: ChatGrade = ChatGrade.stored(UserDefaults.standard.string(forKey: ChatGrade.defaultsKey)) {
        didSet { UserDefaults.standard.set(grade.rawValue, forKey: ChatGrade.defaultsKey) }
    }

    /// The composer's eve/Muse switch (left of History); hidden unless this Mac has Muse set up.
    @Published private(set) var muse = MuseSwitch() { didSet { relay.museIs(muse.on) } }   // calls name Muse too
    /// How much of the list the top bar and the composer (on the keyboard when it is up) cover.
    @Published private(set) var insets: (top: CGFloat, bottom: CGFloat) = (0, 0)

    let relay: Relay
    /// The conversation: its messages, queue, reactions and topics. The list reads it.
    private(set) lazy var engine: ChatEngine = {
        let e = ChatEngine(door: self)
        e.haptic = { Self.play($0) }
        return e
    }()
    private lazy var transport: ChatTransport = {
        let t = ChatTransport(
            endpoint: { [weak self] in self?.relay.pairing.map { ChatEndpoint(base: $0.base, token: $0.token) } },
            refresh: { [weak self] in await self?.relay.refreshAddress() ?? false })
        t.onBusy = { [weak self] busy in self?.busy = busy }
        return t
    }()
    private var backgroundCancel: Task<Void, Never>?
    private var backgroundTask: UIBackgroundTaskIdentifier = .invalid
    /// A tapped attachment being fetched, and the Quick Look data source while it shows.
    private var opening: Task<Void, Never>?
    private var preview: AttachmentPreview?
    private var requests = 0
    /// How long the app may sit in the background before it stops its streams. Held open by a background task
    /// assertion: without one iOS suspends the app within seconds, the timer never fires, and a stream stays open
    /// to the daemon unread, so the rest of the reply never falls back to Telegram (link_chat.py).
    static let backgroundGrace: Duration = .seconds(25)
    static let stoppedInBackground = "Stopped while Eve Link was in the background."

    init(relay: Relay) {
        self.relay = relay
        super.init()
    }

    /// The app is paired (or paired again): read the conversation from that Mac. Unpaired: nothing to show.
    func start(paired: Bool) {
        transport.cancelAll(as: .error(Self.stoppedInBackground))
        ready = paired
        guard paired else { return }
        engine.start()
        refreshMuse()
    }

    // ---- the composer ------------------------------------------------------------------------------------------

    /// The native composer's words, and what it uploaded first: shown in the owner's bubble, their ids sent with
    /// the turn. A send made while eve replies waits its turn.
    func send(_ text: String, attachments: [ChatUploaded] = []) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard ready, !text.isEmpty || !attachments.isEmpty, !ChatLimits.tooLong(text),
              ChatAttachments.valid(attachments.map(\.id)) else { return }
        engine.send(text, attachments: attachments.map {
            ChatAttachment(id: $0.id, name: $0.name, kind: ChatAttachment.Kind(rawValue: $0.kind) ?? .file,
                           thumb: $0.thumb.flatMap { ChatAttachment.isThumb($0) ? $0 : nil })
        })
    }

    /// One attachment, uploaded through the tunnel with the link token (POST /api/link/upload).
    func upload(data: Data, name: String, mime: String) async -> Result<ChatUploaded, ChatFailure> {
        await transport.upload(data: data, name: name, mime: mime)
    }

    /// The native Stop button: this app's streams end now and the reply is treated as finished.
    func stop() {
        transport.cancelTurns(as: .done)
    }

    /// The composer's history button (Horizontal History, docs/app-chat.md): open or close the card row.
    func toggleHistory() {
        Self.play(.light)
        engine.toggleHistory()
    }

    /// Read the eve/Muse switch from the daemon (when the chat starts and when the app comes back).
    /// A read never cancels a flip in flight (the flip's answer is the newer state); a flip cancels a read.
    func refreshMuse() {
        guard !transport.inFlight(Self.museSet) else { return }
        museCall(nil)
    }

    /// The composer's switch: the next words go to Muse, or back to eve. Shown flipped at once; the daemon's answer wins.
    func toggleMuse() {
        Self.play(.light)
        let next = !muse.on
        muse.on = next
        transport.cancel(id: Self.museRead)
        museCall(next)
    }

    private static let museRead = "muse"
    private static let museSet = "muse-set"

    private func museCall(_ on: Bool?) {
        let id = on == nil ? Self.museRead : Self.museSet
        transport.json(id: id, { ChatRequests.muse($0, on: on) }) { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(let json):
                if let state = MuseSwitch.decode(json) { self.muse = state }
            case .failure:
                if on != nil { self.muse.on = !(on ?? false) }      // the flip did not reach eve: show it as it was
            }
        }
    }

    /// The native chrome over the full-screen list (docs/app-chat.md, "The cut-off"): `top` is the top bar's bottom
    /// edge, `bottom` the distance from the composer's top edge (on the keyboard when it is up) to the list's
    /// bottom. The list pads itself by them and stays pinned to its end.
    func setInsets(top: CGFloat, bottom: CGFloat) {
        let next = (max(0, (top * 10).rounded() / 10), max(0, (bottom * 10).rounded() / 10))
        guard next.0.isFinite, next.1.isFinite, next != insets else { return }
        insets = next
    }

    // ---- background and foreground -----------------------------------------------------------------------------

    func scene(_ phase: ScenePhase) {
        switch phase {
        case .background:
            engine.setVisible(false)                       // the long poll stops; a queued send waits
            backgroundCancel?.cancel()
            endBackgroundTask()
            // The grace runs under a background task assertion, so it really ends: the streams are cancelled at its
            // end or when iOS expires the assertion, whichever is first. The daemon then sees the hang-up and sends
            // the rest of the turn to Telegram, so the owner is notified; the long poll reads it back later.
            backgroundTask = UIApplication.shared.beginBackgroundTask(withName: "chat-streams") { [weak self] in
                MainActor.assumeIsolated {
                    self?.backgroundCancel?.cancel()
                    self?.stopStreamsInBackground("background time ran out")
                }
            }
            backgroundCancel = Task { [weak self] in
                try? await Task.sleep(for: Self.backgroundGrace)
                guard !Task.isCancelled, let self else { return }
                self.stopStreamsInBackground("in the background too long")
            }
        case .active:
            backgroundCancel?.cancel()
            backgroundCancel = nil
            endBackgroundTask()
            engine.setVisible(true)
            if ready { refreshMuse() }                     // Muse may have been switched from another door
        default:
            break
        }
    }

    private func stopStreamsInBackground(_ why: String) {
        diag("chat: \(why); streams stopped")
        transport.cancelAll(as: .error(Self.stoppedInBackground))
        endBackgroundTask()
    }

    private func endBackgroundTask() {
        guard backgroundTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTask)
        backgroundTask = .invalid
    }

    // ---- taps in the list --------------------------------------------------------------------------------------

    /// What the list asks the app for: a link, or a photo or file full size.
    var actions: ChatActions {
        ChatActions(openURL: { [weak self] in self?.open($0) },
                    openAttachment: { [weak self] in self?.openAttachment(id: $0.id, name: $0.name, kind: $0.kind.rawValue) })
    }

    /// A tapped photo or file in the chat (the owner's or eve's): fetched full size with the link token
    /// (GET /api/link/file/<id>), written under a folder named by its id, and shown in Quick Look, whose share
    /// button saves or sends it. A second tap while one loads replaces it; a failure is one short alert.
    private func openAttachment(id: String, name: String, kind: String) {
        guard ChatAttachments.isID(id), ChatFiles.kinds.contains(kind) else { return }
        Self.play(.light)
        opening?.cancel()
        opening = Task { [weak self] in
            guard let self else { return }
            let result = await self.transport.download(id: id)
            guard !Task.isCancelled else { return }
            switch result {
            case .success(let file):
                do {
                    let safe = ChatFiles.safeName(String(name.prefix(ChatFiles.maxName)), kind: kind, mime: file.mime)
                    self.showPreview(try AttachmentPreview.write(file.data, id: id, name: safe))
                } catch {
                    diag("chat: an attachment could not be written (\(type(of: error)))")
                    self.alert("Couldn't open that file.")
                }
            case .failure(let f):
                diag("chat: an attachment did not open: \(f.message)")
                self.alert(f.message)
            }
        }
    }

    private func showPreview(_ url: URL) {
        guard let top = Self.topController() else { return }
        let source = AttachmentPreview(url) { [weak self] in self?.preview = nil }
        preview = source
        let ql = QLPreviewController()
        ql.dataSource = source
        ql.delegate = source
        top.present(ql, animated: true)
    }

    private func alert(_ message: String) {
        guard let top = Self.topController() else { return }
        let a = UIAlertController(title: nil, message: message, preferredStyle: .alert)
        a.addAction(UIAlertAction(title: "OK", style: .default))
        top.present(a, animated: true)
    }

    static func play(_ style: ChatHaptic) {
        switch style {
        case .light: UIImpactFeedbackGenerator(style: .light).impactOccurred()
        case .soft: UIImpactFeedbackGenerator(style: .soft).impactOccurred()
        }
    }

    private func open(_ url: URL) {
        switch ChatLinks.external(url) {
        case .safari:
            // Owner, 2026-10-16: "it goes to website and then i have to click open app". A web link opens straight in
            // its own app when the site claims it (a universal link: the NYT, Slack, Notion...); only a page no
            // installed app claims opens in Safari inside Eve Link.
            UIApplication.shared.open(url, options: [.universalLinksOnly: true]) { opened in
                guard !opened, let top = Self.topController() else { return }
                let safari = SFSafariViewController(url: url)
                safari.dismissButtonStyle = .done
                top.present(safari, animated: true)
            }
        case .system:
            UIApplication.shared.open(url)
        case nil:
            diag("chat: a link was not opened (\(url.scheme ?? "?"))")
        }
    }

    /// The controller on top of the app's key window.
    private static func topController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let window = scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? scenes.first?.windows.first
        var top = window?.rootViewController
        while let next = top?.presentedViewController { top = next }
        return top
    }
}

/// The engine's way to the Mac: every request rides ChatTransport, under an id of its own.
extension ChatModel: ChatDoor {
    private func requestID(_ kind: String) -> String {
        requests += 1
        return "\(kind)-\(requests)"
    }

    func turn(id: String, text: String, clientID: String, attachments: [String],
              onChunk: @escaping @MainActor (String) -> Void, onEnd: @escaping @MainActor (TurnEnd) -> Void) {
        // The dial's choice is the app's to add.
        transport.turn(id: id, text: text, clientID: clientID, grade: grade, attachments: attachments,
                       onChunk: { chunk in onChunk(chunk) },
                       onEnd: { ending in
                           switch ending {
                           case .done: onEnd(.done)
                           case .error(let message):
                               onEnd(message == Self.stoppedInBackground ? .interrupted : .failed(message))
                           case .silent: onEnd(.interrupted)
                           }
                       })
    }

    func messages(after: String?, wait: Int, before: String?) async -> Result<String, DoorFailure> {
        await withCheckedContinuation { done in
            transport.messages(id: requestID("messages"), after: after, wait: wait, before: before) { result in
                done.resume(returning: result.mapError { DoorFailure($0.message) })
            }
        }
    }

    func topics() async -> Result<String, DoorFailure> {
        await json("topics") { ChatRequests.topics($0) }
    }

    func bookmark(topic: String, on: Bool) async -> Result<String, DoorFailure> {
        await json("bookmark") { ChatRequests.bookmark($0, topic: topic, on: on) }
    }

    private func json(_ kind: String, _ build: @escaping (ChatEndpoint) -> URLRequest) async -> Result<String, DoorFailure> {
        await withCheckedContinuation { done in
            transport.json(id: requestID(kind), build) { result in
                done.resume(returning: result.mapError { DoorFailure($0.message) })
            }
        }
    }
}

/// One attachment in Quick Look (images, PDFs, text, Office documents; anything else shows its name and size), with
/// Quick Look's own share button. The file lives in tmp/chat-open/<id>/ while it shows and is deleted on dismiss; only
/// one is kept at a time.
@MainActor
final class AttachmentPreview: NSObject, QLPreviewControllerDataSource, QLPreviewControllerDelegate {
    let url: URL
    private let done: () -> Void

    init(_ url: URL, done: @escaping () -> Void) {
        self.url = url
        self.done = done
    }

    static var folder: URL { FileManager.default.temporaryDirectory.appendingPathComponent("chat-open", isDirectory: true) }

    /// The bytes at tmp/chat-open/<id>/<name>: the id is 32 hex (ChatAttachments.isID, checked before the fetch) and
    /// the name one safe component (ChatFiles.safeName). Whatever was open before is removed first.
    static func write(_ data: Data, id: String, name: String) throws -> URL {
        guard ChatAttachments.isID(id) else { throw CocoaError(.fileWriteInvalidFileName) }
        let fm = FileManager.default
        try? fm.removeItem(at: folder)
        let dir = folder.appendingPathComponent(id, isDirectory: true)
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent(name, isDirectory: false)
        try data.write(to: url, options: [.atomic, .completeFileProtection])
        return url
    }

    func numberOfPreviewItems(in controller: QLPreviewController) -> Int { 1 }

    func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> any QLPreviewItem {
        url as NSURL
    }

    /// UIKit calls it on the main thread (the delegate protocol itself is not main-actor isolated).
    nonisolated func previewControllerDidDismiss(_ controller: QLPreviewController) {
        MainActor.assumeIsolated {
            try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
            done()
        }
    }
}
