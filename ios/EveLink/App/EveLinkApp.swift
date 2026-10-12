// Eve Link: talk to eve from the iPhone (docs/action-button.md). One screen (docs/app-chat.md): Chat texts eve,
// Updates (left of it) lists things to act on (docs/updates.md), Work lists the chief's jobs and the owner's apps, voice mode (and the Action Button through TalkIntent) carries each
// turn to eve as a call, and Settings holds pairing and diagnostics.
import SwiftUI
import UIKit
import WidgetKit

@main
struct EveLinkApp: App {
    @StateObject private var relay = PhoneLink.relay
    @StateObject private var log = Diag.shared
    @StateObject private var chat = ChatModel(relay: PhoneLink.relay)
    @StateObject private var work = WorkModel(relay: PhoneLink.relay)
    @StateObject private var updates = UpdatesModel(relay: PhoneLink.relay)
    @State private var talkError: String?
    @Environment(\.scenePhase) private var scenePhase
    @UIApplicationDelegateAdaptor(EveLinkAppDelegate.self) private var appDelegate

    /// Every cold launch opens on Chat, whatever tab was last shown (owner, 2026-10-12: "when I open, chat should
    /// still be main pane"). A link that opens the app (evelink://updates, the widget's evelink://work) still wins:
    /// onOpenURL runs after this.
    init() {
        UserDefaults.standard.set(HomeMode.chat.rawValue, forKey: "home.mode")
    }

    var body: some Scene {
        WindowGroup {
            HomeScreen(relay: relay, chat: chat, work: work, updates: updates, log: log, talkError: $talkError)
                .onChange(of: scenePhase) { _, now in
                    diag("app: \(now)")
                    chat.scene(now)
                    LocationSharer.shared.scene(now)
                    if now == .active { HomeWidget.reload() }
                }
                #if DEBUG && targetEnvironment(simulator)
                .task {
                    // The simulator's design hook: EVE_PAIR_URL pairs as evelink://pair would, with no dialog.
                    if let url = DesignHook.pairURL, case .pair(let link)? = LinkRoute(url) { _ = relay.pair(with: link) }
                }
                #endif
                .onOpenURL { url in
                    switch LinkRoute(url) {
                    case .pair(let link):
                        if relay.pair(with: link) {
                            HomeWidget.reload()
                            LocationSharer.shared.askIfNeeded()     // paired now: the location question can come
                        }
                    case .talk:
                        diag("link: evelink://talk")
                        Task { await talkFromLink() }
                    case .work:
                        // the home-screen widget's tap: the Work view (HomeScreen's @AppStorage("home.mode"))
                        UserDefaults.standard.set(HomeMode.work.rawValue, forKey: "home.mode")
                    case .chat:
                        // a tap on a notification (docs/notifications.md): the Chat
                        UserDefaults.standard.set(HomeMode.chat.rawValue, forKey: "home.mode")
                    case .updates:
                        // evelink://updates (a Shortcut, say): the Updates pane, left of the Chat
                        UserDefaults.standard.set(HomeMode.updates.rawValue, forKey: "home.mode")
                    case nil: break
                    }
                }
        }
    }

    /// evelink://talk (a Shortcuts "Open URLs" action, for example): one press, as the Action Button.
    private func talkFromLink() async {
        if relay.phone?.micAllowed == nil { _ = await relay.phone?.requestMic() }
        do {
            try relay.press()
            talkError = nil
        } catch {
            talkError = (error as? PhoneTurnError)?.description ?? error.localizedDescription
        }
    }
}

/// The app delegate, for one thing SwiftUI does not give: the launch options. iOS relaunches the app in the
/// background for a significant location change (LocationSharer.swift), and the location manager must be set up
/// before didFinishLaunching returns for that fix to arrive.
final class EveLinkAppDelegate: NSObject, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        LocationSharer.shared.launch(
            endpoint: { PhoneLink.relay.pairing.map { ChatEndpoint(base: $0.base, token: $0.token) } },
            refresh: { await PhoneLink.relay.refreshAddress() },
            forLocation: launchOptions?[.location] != nil)
        return true
    }
}

/// The home-screen widget (docs/widget.md) reads eve itself every 15 minutes; the app asks for a fresh read when it
/// comes to the front and when the jobs change, so the widget does not lag what the owner just saw or did.
enum HomeWidget {
    static func reload() { WidgetCenter.shared.reloadTimelines(ofKind: TodayWidgetKind.kind) }
}

#if DEBUG && targetEnvironment(simulator)
/// For screenshots in the simulator only (docs/app-chat.md, "Design hook"); none of it is in a device build.
/// EVE_PAIR_URL (environment) pairs; -eveScreen updates | chat | apps | work | voice | settings | pika opens that screen (pika:
/// the Apps tab with the Pika screen open); -eveGrade
/// auto | instant | think preselects the speed dial, -eveKeyboard YES puts the cursor in the chat's composer (launch
/// arguments, read through UserDefaults). -eveControl notes | spotify | lights | firetv | pika | memory opens that control's sheet over
/// Work; -eveMemoryQuery <words> runs that search in the Memory card; -eveNote <path> (with -eveControl notes)
/// opens that note's page.
/// -eveActivity <phase> puts up the call's Live Activity in that phase.
enum DesignHook {
    static var pairURL: URL? { ProcessInfo.processInfo.environment["EVE_PAIR_URL"].flatMap(URL.init(string:)) }
    static var screen: String? { UserDefaults.standard.string(forKey: "eveScreen") }
    static var grade: String? { UserDefaults.standard.string(forKey: "eveGrade") }
    static var keyboard: Bool { UserDefaults.standard.bool(forKey: "eveKeyboard") }
    static var control: String? { UserDefaults.standard.string(forKey: "eveControl") }
    /// -eveMemoryQuery <words> (with -eveControl memory): runs that search in the Memory card, for screenshots.
    static var memoryQuery: String? { UserDefaults.standard.string(forKey: "eveMemoryQuery") }
    /// -eveNote <vault path> (with -eveControl notes): opens that note's page, for screenshots.
    static var note: String? { UserDefaults.standard.string(forKey: "eveNote") }
    /// -eveActivity listening | thinking | speaking | waiting | notes: shows the call's Live Activity in that phase.
    static var activity: String? { UserDefaults.standard.string(forKey: "eveActivity") }
}
#endif
