// Pika, the owner's robot car (docs/pika.md; owner, 2026-10-10: "can we build a controller for pika as an app",
// "Inside Eve Link, via the Mac", "very minimalist"). The car's live video fills the screen, with four arrows and a
// Stop over it. Hold an arrow to drive; lift it and the car stops. Every press goes through the Mac (pika.py), which
// also stops the car when the beats stop coming. The hold logic and the video's framing are Shared/PikaDrive.swift.
import SwiftUI
import UIKit

struct PikaControls: View {
    let client: WorkClient
    @StateObject private var feed = PikaFeed()
    @State private var hold = PikaHold()
    @State private var beating: Task<Void, Never>?
    @State private var line = ""
    @State private var presses = 0
    @State private var fails = 0
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ZStack {
            Color.black
            video
            VStack(spacing: 16) {
                Spacer()
                if !status.isEmpty {
                    Text(status)
                        .font(.footnote)
                        .foregroundStyle(.white)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(Capsule().fill(Color.black.opacity(0.55)))
                        .padding(.horizontal, 24)
                }
                pad
            }
            .padding(.bottom, 28)
        }
        .ignoresSafeArea(edges: .bottom)
        .task { feed.start(client) }
        .onDisappear { leave() }
        .onChange(of: scenePhase) { _, now in
            if now == .active { feed.start(client) } else { stop(); feed.stop() }
        }
        .onChange(of: status) { _, now in
            if !now.isEmpty { AccessibilityNotification.Announcement(now).post() }
        }
        .sensoryFeedback(.impact(weight: .light), trigger: presses)
        .sensoryFeedback(.error, trigger: fails)
    }

    /// One line, only when something is wrong or still coming: the Mac's last refusal, else the video's.
    private var status: String { line.isEmpty ? (feed.problem ?? "") : line }

    private var video: some View {
        GeometryReader { g in
            if let image = feed.image {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFill()
                    .frame(width: g.size.width, height: g.size.height)
                    .clipped()
                    .accessibilityLabel("Pika's camera")
            } else {
                Image(systemName: "car")
                    .font(.system(size: 40))
                    .foregroundStyle(.white.opacity(0.3))
                    .frame(width: g.size.width, height: g.size.height)
                    .accessibilityHidden(true)
            }
        }
    }

    private var pad: some View {
        VStack(spacing: 6) {
            key(.forward)
            HStack(spacing: 6) {
                key(.left)
                Button(action: stop) {
                    Image(systemName: "stop.fill")
                        .font(.title3.weight(.semibold))
                        .foregroundStyle(.white)
                        .frame(width: 64, height: 64)
                        .background(Circle().fill(Color.red.opacity(0.85)))
                        .overlay(Circle().strokeBorder(Color.white.opacity(0.12)))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Stop")
                key(.right)
            }
            key(.back)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Drive pad")
    }

    private func key(_ d: PikaDirection) -> some View {
        HoldKey(direction: d, driving: hold.direction == d,
                onPress: { press(d) }, onRelease: { release(d) })
    }

    // ---- driving ------------------------------------------------------------------------------------------------

    private func press(_ d: PikaDirection) {
        presses += 1
        line = ""
        send(hold.press(d, id: UUID().uuidString.lowercased()))
        beating?.cancel()
        beating = Task { @MainActor in
            while !Task.isCancelled {
                try? await Task.sleep(for: PikaHold.beat)
                guard !Task.isCancelled, let beat = hold.renew() else { return }
                send(beat)
            }
        }
    }

    private func release(_ d: PikaDirection) {
        guard let stop = hold.release(d) else { return }
        beating?.cancel()
        beating = nil
        send(stop)
    }

    private func stop() {
        beating?.cancel()
        beating = nil
        send(hold.stop())
    }

    /// Leaving the screen: stop, and let the Mac let go of the car and its video at once.
    private func leave() {
        stop()
        feed.stop()
        Task { _ = await client.pika(PikaCommand(action: "close", dir: nil, hold: nil)) }
    }

    /// Every request on its own, never waiting on another: a beat that is slow must not hold up the stop behind it.
    private func send(_ command: PikaCommand) {
        Task { @MainActor in
            switch await client.pika(command) {
            case .success(let r):
                if r.stopped, let id = command.hold {
                    if hold.id == id && hold.holding { fails += 1 }
                    hold.ended(id)
                    if !hold.holding { beating?.cancel(); beating = nil }
                }
                if !r.ok, !r.line.isEmpty { line = r.line }
            case .failure(let f):
                if command.action != "close" {
                    line = f.message.isEmpty ? "eve didn't answer. Is the Mac awake?" : f.message
                }
            }
        }
    }
}

/// One arrow: down drives, up stops. A GestureState resets when the system cancels the touch too, so a release is
/// never missed.
private struct HoldKey: View {
    let direction: PikaDirection
    let driving: Bool
    let onPress: () -> Void
    let onRelease: () -> Void
    @GestureState private var down = false

    var body: some View {
        Image(systemName: direction.symbol)
            .font(.title2.weight(.semibold))
            .foregroundStyle(driving ? Color.black : Color.white)
            .frame(width: 64, height: 64)
            // always over video, so always the dark fill, in light mode too
            .background(Circle().fill(driving ? Color.white : Color(white: 0.11).opacity(0.8)))
            .overlay(Circle().strokeBorder(Color.white.opacity(0.12)))
            .scaleEffect(down ? 0.94 : 1)
            .animation(.easeOut(duration: 0.1), value: down)
            .contentShape(Circle())
            .gesture(DragGesture(minimumDistance: 0).updating($down) { _, state, _ in state = true })
            .onChange(of: down) { _, now in now ? onPress() : onRelease() }
            .accessibilityElement()
            .accessibilityLabel(direction.label)
            .accessibilityHint("Hold to drive. Lift to stop.")
            .accessibilityAddTraits(.isButton)
    }
}

/// The car's video from the Mac, the newest JPEG shown: over WebRTC straight from the Mac (PikaRTC) when it
/// connects, else one multipart stream through the tunnel. Tries again every few seconds while the screen is up, so a
/// car switched on later appears by itself. Nothing connects while the screen is closed.
@MainActor
final class PikaFeed: ObservableObject {
    @Published private(set) var image: UIImage?
    @Published private(set) var problem: String? = "Connecting to Pika…"
    private var session: URLSession?
    private var rtc: PikaRTC?
    private var running = false
    private var retry: Task<Void, Never>?
    private var rtcLatency = PikaLatency(path: "webrtc")
    private var tunnelLatency = PikaLatency(path: "tunnel")

    func start(_ client: WorkClient) {
        guard !running else { return }
        running = true
        connect(client)
    }

    func stop() {
        running = false
        retry?.cancel()
        retry = nil
        rtc?.close()
        rtc = nil
        session?.invalidateAndCancel()
        session = nil
    }

    /// WebRTC first; the tunnel stream when it can't connect.
    private func connect(_ client: WorkClient) {
        guard running else { return }
        let link = PikaRTC { [weak self] event in
            Task { @MainActor in self?.handle(event, client) }
        }
        rtc?.close()
        rtc = link
        Task { @MainActor in
            let ok = await link.connect(client)
            guard running, rtc === link else {
                link.close()
                return
            }
            if !ok {
                rtc = nil
                open(client)
            }
        }
    }

    private func handle(_ event: PikaRTC.Event, _ client: WorkClient) {
        guard running, rtc != nil else { return }
        switch event {
        case .frame(let jpeg, let stamp):
            show(jpeg)
            measure(&rtcLatency, stamp, client)
        case .line(let line):
            problem = line
        case .closed:
            rtc = nil
            problem = image == nil ? "No video from Pika." : "Pika's video stopped."
            again(client)
        }
    }

    private func show(_ jpeg: Data) {
        if let img = UIImage(data: jpeg) {
            image = img
            problem = nil
        }
    }

    private func measure(_ latency: inout PikaLatency, _ stamp: Double?, _ client: WorkClient) {
        guard let stamp, let body = latency.add(stamp: stamp, now: Date().timeIntervalSince1970 * 1000),
              let data = try? JSONSerialization.data(withJSONObject: body) else { return }
        Task { await client.pikaStats(data) }
    }

    private func again(_ client: WorkClient) {
        retry?.cancel()
        retry = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            self?.connect(client)
        }
    }

    private func open(_ client: WorkClient) {
        guard running else { return }
        guard let ep = client.currentEndpoint() else {
            problem = "Eve Link isn't paired."
            return
        }
        let receiver = MJPEGReceiver { [weak self] event in
            Task { @MainActor in self?.handle(event, client) }
        }
        let config = URLSessionConfiguration.default
        config.urlCache = nil
        config.httpCookieStorage = nil
        config.timeoutIntervalForRequest = 15
        let s = URLSession(configuration: config, delegate: receiver, delegateQueue: nil)
        session?.invalidateAndCancel()
        session = s
        s.dataTask(with: ControlsRequests.pikaStream(ep)).resume()
    }

    private func handle(_ event: MJPEGReceiver.Event, _ client: WorkClient) {
        guard running, session != nil else { return }
        switch event {
        case .frame(let data, let stamp):
            show(data)
            measure(&tunnelLatency, stamp, client)
        case .ended(let line):
            problem = line ?? (image == nil ? "No video from Pika." : "Pika's video stopped.")
            session?.finishTasksAndInvalidate()
            session = nil
            again(client)                                  // WebRTC first again: the Mac or the Wi-Fi may be back
        }
    }
}

/// URLSession's delegate for the stream: frames as they complete, and why the stream ended. The camera's picture
/// (ObsbotView.swift) uses it too, with its own fallback line.
final class MJPEGReceiver: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    enum Event: Sendable {
        case frame(Data, Double?)          // the JPEG and its X-Stamp
        case ended(String?)
    }

    private let emit: @Sendable (Event) -> Void
    private let lock = NSLock()
    private var splitter = MJPEGSplitter()
    private var refused: Data?             // a non-200 answer's body, for its line
    private let fallback: String

    init(fallback: String = "Pika's video isn't available.", emit: @escaping @Sendable (Event) -> Void) {
        self.fallback = fallback
        self.emit = emit
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping @Sendable (URLSession.ResponseDisposition) -> Void) {
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        lock.withLock { refused = status == 200 ? nil : Data() }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        let frame: (Data, Double?)? = lock.withLock {
            if refused != nil {
                if refused!.count < 4096 { refused!.append(data) }
                return nil
            }
            return splitter.feed(data).map { ($0, splitter.stamp) }
        }
        if let frame { emit(.frame(frame.0, frame.1)) }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if (error as? URLError)?.code == .cancelled { return }
        let body = lock.withLock { refused }
        let obj = body.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
        let line = (obj?["line"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            ?? (body != nil ? fallback : nil)
        emit(.ended(line))
    }
}
