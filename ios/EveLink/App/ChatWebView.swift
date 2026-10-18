// The chat's message list: OpenUI (github.com/thesysdev/openui) in a WKWebView, from App/ChatWeb in the app bundle
// (docs/app-chat.md). The web view never loads anything from the network and never navigates away; the page talks to
// the app only through the "eve" message handler, and the app answers on window.eveNative.
import QuickLook
import SafariServices
import SwiftUI
import UIKit
import WebKit

/// The web view, its bridge and the transport, kept for the app's life so switching to Work never reloads the page.
@MainActor
final class ChatModel: NSObject, ObservableObject {
    /// The page posted {type:"ready"}: the composer may send.
    @Published private(set) var ready = false
    /// A turn from this app is streaming: the send button is a stop button.
    @Published private(set) var busy = false
    /// The composer's speed dial: sent with every turn as "grade", and remembered.
    @Published var grade: ChatGrade = ChatGrade.stored(UserDefaults.standard.string(forKey: ChatGrade.defaultsKey)) {
        didSet { UserDefaults.standard.set(grade.rawValue, forKey: ChatGrade.defaultsKey) }
    }

    /// The composer's eve/Muse switch (left of History); hidden unless this Mac has Muse set up.
    @Published private(set) var muse = MuseSwitch()

    let relay: Relay
    private(set) lazy var webView: WKWebView = makeWebView()
    private lazy var transport: ChatTransport = {
        let t = ChatTransport(
            endpoint: { [weak self] in self?.relay.pairing.map { ChatEndpoint(base: $0.base, token: $0.token) } },
            refresh: { [weak self] in await self?.relay.refreshAddress() ?? false })
        t.onBusy = { [weak self] busy in self?.setBusy(busy) }
        return t
    }()
    private var backgroundCancel: Task<Void, Never>?
    private var backgroundTask: UIBackgroundTaskIdentifier = .invalid
    private var revealFallback: Task<Void, Never>?
    /// A tapped attachment being fetched, and the Quick Look data source while it shows.
    private var opening: Task<Void, Never>?
    private var preview: AttachmentPreview?
    /// Where the top bar and the composer cover the page, last sent (or waiting for the page's ready).
    private var insets: (top: Double, bottom: Double)?
    /// How long the app may sit in the background before it stops its streams. Held open by a background task
    /// assertion: without one iOS suspends the app within seconds, the timer never fires, and a stream stays open
    /// to the daemon unread, so the rest of the reply never falls back to Telegram (link_chat.py).
    static let backgroundGrace: Duration = .seconds(25)

    init(relay: Relay) {
        self.relay = relay
        super.init()
    }

    // ---- native -> page ---------------------------------------------------------------------------------------

    /// The native composer's words: the page runs them through OpenUI as if typed in its own composer. `attachments`:
    /// what the composer uploaded first; the page shows them in the owner's bubble and sends their ids with the turn.
    func send(_ text: String, attachments: [ChatUploaded] = []) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard ready, !text.isEmpty || !attachments.isEmpty, !ChatBridgeMessage.tooLong(text),
              ChatAttachments.valid(attachments.map(\.id)) else { return }
        if attachments.isEmpty {
            js("send", [ChatJS.literal(text)])
        } else {
            js("send", [ChatJS.literal(text), ChatJS.literal(ChatAttachments.pageJSON(attachments))])
        }
    }

    /// One attachment, uploaded through the tunnel with the link token (POST /api/link/upload).
    func upload(data: Data, name: String, mime: String) async -> Result<ChatUploaded, ChatFailure> {
        await transport.upload(data: data, name: name, mime: mime)
    }

    /// The native Stop button: this app's streams end now and the page treats the reply as finished.
    func stop() {
        transport.cancelTurns(as: .done)
    }

    /// The composer's history button (Horizontal History, docs/app-chat.md): the page opens or closes its card row.
    func toggleHistory() {
        Self.play(.light)
        js("toggleHistory")
    }

    /// Read the eve/Muse switch from the daemon (when the chat becomes ready and when the app comes back).
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

    func scrollToBottom() {
        guard ready else { return }
        js("scrollToBottom")
    }

    /// The native chrome over the full-screen web view (docs/app-chat.md, "The cut-off"): `top` is the top bar's bottom
    /// edge, `bottom` the distance from the composer's top edge (on the keyboard when it is up) to the web view's
    /// bottom. Sent on every change; the page pads its list by them and keeps it pinned to the bottom.
    func setInsets(top: CGFloat, bottom: CGFloat) {
        let next = ((Double(top) * 10).rounded() / 10, (Double(bottom) * 10).rounded() / 10)
        if let insets, insets == next { return }
        insets = next
        sendInsets()
    }

    private func sendInsets() {
        guard ready, let insets else { return }
        js("setInsets", [ChatJS.insets(top: insets.top, bottom: insets.bottom)])
    }

    func scene(_ phase: ScenePhase) {
        switch phase {
        case .background:
            backgroundCancel?.cancel()
            endBackgroundTask()
            // The grace runs under a background task assertion, so it really ends: the streams are cancelled at its
            // end or when iOS expires the assertion, whichever is first. The daemon then sees the hang-up and sends
            // the rest of the turn to Telegram, so the owner is notified; the page's poll reads it back later.
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
            if ready { refreshMuse() }                     // Muse may have been switched from another door
        default:
            break
        }
    }

    private func stopStreamsInBackground(_ why: String) {
        diag("chat: \(why); streams stopped")
        transport.cancelAll(as: .error("Stopped while Eve Link was in the background."))
        endBackgroundTask()
    }

    private func endBackgroundTask() {
        guard backgroundTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTask)
        backgroundTask = .invalid
    }

    private func setBusy(_ busy: Bool) {
        self.busy = busy
        js("setBusy", [ChatJS.literal(busy)])
    }

    private func js(_ fn: String, _ args: [String] = []) {
        webView.evaluateJavaScript(ChatJS.call(fn, args)) { _, error in
            if let error { diag("chat: js \(fn) failed: \(error.localizedDescription)") }
        }
    }

    // ---- page -> native ---------------------------------------------------------------------------------------

    fileprivate func received(_ body: Any) {
        guard let msg = ChatBridgeMessage(body: body) else {
            diag("chat: dropped a message the bridge does not know")
            if let refused = ChatBridgeMessage.refusedTurn(body) {
                js("onError", [ChatJS.literal(refused.id), ChatJS.literal(refused.line)])
            }
            return
        }
        handle(msg)
    }

    func handle(_ msg: ChatBridgeMessage) {
        switch msg {
        case .ready:
            ready = true
            refreshMuse()
            sendInsets()
            reveal()
            if busy { js("setBusy", [ChatJS.literal(true)]) }
        case let .turn(id, text, clientID, attachments):
            let lit = ChatJS.literal(id)
            // The dial's choice is native's to add; the attachment ids come from the page's message.
            transport.turn(id: id, text: text, clientID: clientID, grade: grade, attachments: attachments,
                           onChunk: { [weak self] chunk in self?.js("onChunk", [lit, ChatJS.literal(chunk)]) },
                           onEnd: { [weak self] ending in
                               switch ending {
                               case .done: self?.js("onDone", [lit])
                               case .error(let m): self?.js("onError", [lit, ChatJS.literal(m)])
                               case .silent: break
                               }
                           })
        case .cancel(let id):
            transport.cancel(id: id, as: .silent)
        case let .messages(id, after, wait, before):
            let lit = ChatJS.literal(id)
            transport.messages(id: id, after: after, wait: wait, before: before) { [weak self] result in
                switch result {
                case .success(let json): self?.js("onMessages", [lit, ChatJS.literal(json)])
                case .failure(let f): self?.js("onError", [lit, ChatJS.literal(f.message)])
                }
            }
        case .topics(let id):
            linkJSON(id) { ChatRequests.topics($0) }
        case let .bookmark(id, topic, on):
            linkJSON(id) { ChatRequests.bookmark($0, topic: topic, on: on) }
        case .haptic(let style):
            Self.play(style)
        case .openURL(let url):
            open(url)
        case let .openAttachment(id, name, kind):
            openAttachment(id: id, name: name, kind: kind)
        }
    }

    /// Horizontal History's requests: the daemon's JSON goes back to the page as text (onLink), a failure as onError.
    private func linkJSON(_ id: String, _ build: @escaping (ChatEndpoint) -> URLRequest) {
        let lit = ChatJS.literal(id)
        transport.json(id: id, build) { [weak self] result in
            switch result {
            case .success(let json): self?.js("onLink", [lit, ChatJS.literal(json)])
            case .failure(let f): self?.js("onError", [lit, ChatJS.literal(f.message)])
            }
        }
    }

    /// A tapped photo or file in the chat (the owner's or eve's): fetched full size with the link token
    /// (GET /api/link/file/<id>), written under a folder named by its id, and shown in Quick Look, whose share
    /// button saves or sends it. A second tap while one loads replaces it; a failure is one short alert.
    private func openAttachment(id: String, name: String, kind: String) {
        Self.play(.light)
        opening?.cancel()
        opening = Task { [weak self] in
            guard let self else { return }
            let result = await self.transport.download(id: id)
            guard !Task.isCancelled else { return }
            switch result {
            case .success(let file):
                do {
                    let url = try AttachmentPreview.write(file.data, id: id,
                                                          name: ChatFiles.safeName(name, kind: kind, mime: file.mime))
                    self.showPreview(url)
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
        guard let top = Self.topController(webView.window?.rootViewController) else { return }
        let source = AttachmentPreview(url) { [weak self] in self?.preview = nil }
        preview = source
        let ql = QLPreviewController()
        ql.dataSource = source
        ql.delegate = source
        top.present(ql, animated: true)
    }

    private func alert(_ message: String) {
        guard let top = Self.topController(webView.window?.rootViewController) else { return }
        let a = UIAlertController(title: nil, message: message, preferredStyle: .alert)
        a.addAction(UIAlertAction(title: "OK", style: .default))
        top.present(a, animated: true)
    }

    static func play(_ style: ChatHaptic) {
        switch style {
        case .light: UIImpactFeedbackGenerator(style: .light).impactOccurred()
        case .soft: UIImpactFeedbackGenerator(style: .soft).impactOccurred()
        case .success: UINotificationFeedbackGenerator().notificationOccurred(.success)
        }
    }

    private func open(_ url: URL) {
        switch ChatLinks.external(url) {
        case .safari:
            // Owner, 2026-10-16: "it goes to website and then i have to click open app". A web link opens straight in
            // its own app when the site claims it (a universal link: the NYT, Slack, Notion...); only a page no
            // installed app claims opens in Safari inside Eve Link.
            UIApplication.shared.open(url, options: [.universalLinksOnly: true]) { [weak self] opened in
                guard !opened, let self,
                      let top = Self.topController(self.webView.window?.rootViewController) else { return }
                let safari = SFSafariViewController(url: url)
                safari.dismissButtonStyle = .done
                top.present(safari, animated: true)
            }
        case .system:
            UIApplication.shared.open(url)
        case nil:
            break
        }
    }

    private static func topController(_ root: UIViewController?) -> UIViewController? {
        var top = root
        while let next = top?.presentedViewController { top = next }
        return top
    }

    // ---- the web view -----------------------------------------------------------------------------------------

    private func makeWebView() -> WKWebView {
        let config = WKWebViewConfiguration()
        config.setURLSchemeHandler(ChatSchemeHandler(), forURLScheme: ChatNavigation.scheme)
        config.userContentController.add(WeakMessageHandler(self), name: "eve")
        config.userContentController.addUserScript(WKUserScript(source: Self.baseline, injectionTime: .atDocumentEnd,
                                                                forMainFrameOnly: true))
        config.allowsInlineMediaPlayback = true
        config.dataDetectorTypes = []
        config.suppressesIncrementalRendering = true
        config.defaultWebpagePreferences.preferredContentMode = .mobile

        let web = WKWebView(frame: .zero, configuration: config)
        web.isOpaque = false
        web.backgroundColor = .clear
        web.underPageBackgroundColor = EveStyle.background   // pure black in dark mode, as the screen behind it
        web.allowsLinkPreview = false
        web.allowsBackForwardNavigationGestures = false
        web.navigationDelegate = self
        web.uiDelegate = self
        web.alpha = 0                                   // shown on "ready", so no half-drawn page ever shows
        let scroll = web.scrollView
        scroll.contentInsetAdjustmentBehavior = .never  // the page pads with env(safe-area-inset-*)
        scroll.bounces = false
        scroll.alwaysBounceVertical = false
        scroll.alwaysBounceHorizontal = false
        scroll.backgroundColor = .clear
        scroll.minimumZoomScale = 1
        scroll.maximumZoomScale = 1
        scroll.pinchGestureRecognizer?.isEnabled = false
        scroll.keyboardDismissMode = .interactive
        #if DEBUG
        web.isInspectable = true
        #endif
        web.load(URLRequest(url: ChatNavigation.start))
        revealFallback = Task { [weak self] in        // a page that never says ready still shows, not a blank tab
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled else { return }
            self?.reveal()
        }
        return web
    }

    private func reveal() {
        revealFallback?.cancel()
        guard webView.alpha < 1 else { return }
        UIView.animate(withDuration: 0.12) { self.webView.alpha = 1 }
    }

    /// Native defaults the page builds on, put first in <head> so the page's own CSS wins: no tap flash, no long-press
    /// callout or selection outside the text the page marks selectable, the system font, light and dark. Zoom is off
    /// natively (the scroll view's zoom scale is fixed at 1).
    private static let baseline = """
    (function(){
      var s=document.createElement('style');s.id='eve-native-baseline';
      s.textContent=':where(html){-webkit-text-size-adjust:100%;-webkit-tap-highlight-color:transparent;'+
        'font-family:-apple-system,system-ui,sans-serif;color-scheme:light dark;}'+
        ':where(body){-webkit-touch-callout:none;-webkit-user-select:none;user-select:none;overscroll-behavior:none;}';
      var h=document.head||document.documentElement;
      h.insertBefore(s,h.firstChild);
    })();
    """
}

extension ChatModel: WKNavigationDelegate, WKUIDelegate {
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void) {
        switch ChatNavigation.decide(action.request.url, linkActivated: action.navigationType == .linkActivated) {
        case .allow:
            decisionHandler(.allow)
        case .open(let url):
            decisionHandler(.cancel)
            open(url)
        case .refuse:
            diag("chat: navigation refused (\(action.request.url?.scheme ?? "?"))")
            decisionHandler(.cancel)
        }
    }

    /// target=_blank and window.open: never a second web view; a link goes to Safari.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = action.request.url, ChatLinks.external(url) != nil { open(url) }
        return nil
    }

    /// No long-press menu or link preview on anything in the page.
    func webView(_ webView: WKWebView, contextMenuConfigurationFor elementInfo: WKContextMenuElementInfo)
        async -> UIContextMenuConfiguration? {
        nil
    }

    /// The web content process died (memory pressure): load the page again; it asks for the history itself.
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        diag("chat: web content process ended; reloading")
        ready = false
        transport.cancelAll(as: .silent)
        webView.load(URLRequest(url: ChatNavigation.start))
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

/// WKUserContentController keeps its handlers strongly; this keeps the model weakly so the page cannot leak it.
private final class WeakMessageHandler: NSObject, WKScriptMessageHandler {
    weak var model: ChatModel?
    init(_ model: ChatModel) { self.model = model }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        // Only the bundled page, main frame, may talk to the app.
        guard message.frameInfo.isMainFrame, message.frameInfo.securityOrigin.protocol == ChatNavigation.scheme
        else { return }
        MainActor.assumeIsolated { model?.received(message.body) }
    }
}

/// Serves App/ChatWeb from the app bundle at eveapp://chat/...
final class ChatSchemeHandler: NSObject, WKURLSchemeHandler {
    static var root: URL? { Bundle.main.url(forResource: "ChatWeb", withExtension: nil) }

    func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
        guard let url = task.request.url, let root = Self.root, let file = ChatAssets.resolve(url, root: root),
              let data = try? Data(contentsOf: file, options: .mappedIfSafe) else {
            let url = task.request.url ?? ChatNavigation.start
            task.didReceive(HTTPURLResponse(url: url, statusCode: 404, httpVersion: "HTTP/1.1", headerFields: [:])!)
            task.didFinish()
            return
        }
        let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1",
                                       headerFields: ChatAssets.headers(for: file, length: data.count))!
        task.didReceive(response)
        task.didReceive(data)
        task.didFinish()
    }

    func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {}   // answered synchronously: nothing to stop
}

struct ChatWebView: UIViewRepresentable {
    let model: ChatModel
    func makeUIView(context: Context) -> WKWebView { model.webView }
    func updateUIView(_ view: WKWebView, context: Context) {}
}

