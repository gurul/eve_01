// The OBSBOT webcam (docs/obsbot.md; owner, 2026-10-11: "build in an app into eve for obsbot"). The camera's live
// picture on top; under it, hold-to-turn arrows around a Center key, the zoom, and a row for sleep, tracking and the
// AI mode. Every press goes through the Mac (obsbot_panel.py), which also stops the gimbal when the beats stop. The
// hold logic and the requests are Shared/ObsbotControl.swift; the picture's parser is Pika's (PikaView.swift).
import SwiftUI
import UIKit

struct ObsbotControls: View {
    let client: WorkClient
    @StateObject private var feed = ObsbotFeed()
    @State private var hold = ObsbotHold()
    @State private var beating: Task<Void, Never>?
    @State private var cam: ObsbotReply?
    @State private var zoom: Double = 0
    @State private var zooming = false
    @State private var tracking = false
    @State private var line = ""
    @State private var failed = false
    @State private var busy = false
    @State private var taps = 0
    @State private var fails = 0
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ScrollView {
            VStack(spacing: 22) {
                picture
                SaidLine(text: line.isEmpty ? where_ : line, error: failed)
                pad
                zoomRow
                actions
                if let presets = cam?.presets, !presets.isEmpty { presetRow(presets) }
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
        }
        .scrollDisabled(hold.holding)
        .task {
            feed.start(client)
            await status()
        }
        .onDisappear { leave() }
        .onChange(of: scenePhase) { _, now in
            if now == .active { feed.start(client) } else { stopTurning(); feed.stop() }
        }
        .sensoryFeedback(.impact(weight: .light), trigger: taps)
        .sensoryFeedback(.error, trigger: fails)
    }

    /// Where the camera points, from the last read: the line under the picture when nothing went wrong.
    private var where_: String {
        guard let cam, cam.ok else { return "" }
        if cam.awake == false { return "\(cam.camera ?? "The camera") is asleep." }
        guard let pan = cam.pan, let pitch = cam.pitch else { return cam.camera ?? "" }
        // the camera's pitch is + down; the owner reads tilt as + up
        return "Pan \(pan)°, tilt \(-pitch)°" + (cam.zoom.map { $0 > 0 ? ", zoom \($0)%" : "" } ?? "")
    }

    // ---- the picture ----------------------------------------------------------------------------------------------

    private var picture: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 16, style: .continuous).fill(Color.black)
            if let image = feed.image {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFit()
                    .accessibilityLabel("The camera's picture")
            } else {
                VStack(spacing: 10) {
                    Image(systemName: "web.camera")
                        .font(.system(size: 34))
                        .foregroundStyle(.white.opacity(0.35))
                    if let problem = feed.problem {
                        Text(problem)
                            .font(.footnote)
                            .foregroundStyle(.white.opacity(0.75))
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 20)
                    }
                }
                .accessibilityElement(children: .combine)
            }
        }
        .aspectRatio(16 / 9, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
    }

    // ---- turning ----------------------------------------------------------------------------------------------------

    private var pad: some View {
        VStack(spacing: 6) {
            key(.up)
            HStack(spacing: 6) {
                key(.left)
                Button { act(ObsbotCommand(action: "center"), busyLine: "Centering…") } label: {
                    Image(systemName: "scope")
                        .font(.title3.weight(.semibold))
                        .foregroundStyle(Color.primary)
                        .frame(width: 64, height: 64)
                        .background(Circle().fill(Color.eveFill))
                        .overlay(Circle().strokeBorder(Color.eveEdge))
                }
                .buttonStyle(PressStyle())
                .accessibilityLabel("Center the camera")
                key(.right)
            }
            key(.down)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Turn pad")
    }

    private func key(_ d: ObsbotDirection) -> some View {
        TurnKey(direction: d, turning: hold.direction == d, onPress: { press(d) }, onRelease: { release(d) })
    }

    private func press(_ d: ObsbotDirection) {
        taps += 1
        line = ""
        failed = false
        send(hold.press(d, id: UUID().uuidString.lowercased()))
        beating?.cancel()
        beating = Task { @MainActor in
            while !Task.isCancelled {
                try? await Task.sleep(for: ObsbotHold.beat)
                guard !Task.isCancelled, let beat = hold.renew() else { return }
                send(beat)
            }
        }
    }

    private func release(_ d: ObsbotDirection) {
        guard let stop = hold.release(d) else { return }
        beating?.cancel()
        beating = nil
        send(stop)
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(400))      // the gimbal settles, then where it ended up
            await status()
        }
    }

    private func stopTurning() {
        beating?.cancel()
        beating = nil
        if hold.holding { send(hold.stop()) }
    }

    private func leave() {
        stopTurning()
        feed.stop()
        Task { _ = await client.obsbot(ObsbotCommand(action: "close")) }
    }

    /// A beat or a stop on its own, never waiting on another request.
    private func send(_ command: ObsbotCommand) {
        Task { @MainActor in
            switch await client.obsbot(command) {
            case .success(let r):
                if r.stopped, let id = command.hold {
                    if hold.id == id && hold.holding { fails += 1 }
                    hold.ended(id)
                    if !hold.holding { beating?.cancel(); beating = nil }
                }
                if !r.ok, !r.line.isEmpty { say(r.line, error: true) }
            case .failure(let f):
                if command.action != "close" { say(f.message.isEmpty ? noAnswer : f.message, error: true) }
            }
        }
    }

    // ---- zoom, sleep, tracking, mode, presets -----------------------------------------------------------------------

    private var zoomRow: some View {
        HStack(spacing: 12) {
            Image(systemName: "minus.magnifyingglass").foregroundStyle(.secondary).accessibilityHidden(true)
            Slider(value: $zoom, in: 0...100, step: 1) { editing in
                zooming = editing
                if !editing { act(ObsbotCommand(action: "zoom", level: Int(zoom))) }
            }
            .accessibilityLabel("Zoom")
            .accessibilityValue("\(Int(zoom)) percent")
            Image(systemName: "plus.magnifyingglass").foregroundStyle(.secondary).accessibilityHidden(true)
        }
    }

    private var actions: some View {
        let asleep = cam?.awake == false
        return HStack(spacing: 10) {
            chip(asleep ? "Wake" : "Sleep", symbol: asleep ? "sun.max" : "moon", on: false) {
                act(ObsbotCommand(action: asleep ? "wake" : "sleep"), busyLine: asleep ? "Waking…" : "Going to sleep…")
            }
            chip("Track", symbol: "person.crop.square", on: tracking) {
                tracking.toggle()
                act(ObsbotCommand(action: "track", on: tracking))
            }
            Menu {
                ForEach(cam?.modes ?? [], id: \.self) { name in
                    Button(ObsbotModes.title(name)) { act(ObsbotCommand(action: "mode", name: name)) }
                }
            } label: {
                chipLabel("Mode", symbol: "sparkles", on: false)
            }
            .disabled((cam?.modes ?? []).isEmpty)
            .accessibilityLabel("AI mode")
        }
    }

    private func chip(_ title: String, symbol: String, on: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) { chipLabel(title, symbol: symbol, on: on) }
            .buttonStyle(PressStyle())
            .disabled(busy)
            .accessibilityAddTraits(on ? .isSelected : [])
    }

    private func chipLabel(_ title: String, symbol: String, on: Bool) -> some View {
        Label(title, systemImage: symbol)
            .font(.subheadline.weight(.medium))
            .foregroundStyle(on ? Color.eveBackground : Color.primary)
            .frame(maxWidth: .infinity, minHeight: 44)
            .background(Capsule().fill(on ? Color.primary : Color.eveFill))
    }

    private func presetRow(_ presets: [ObsbotPreset]) -> some View {
        HStack(spacing: 8) {
            ForEach(presets) { p in
                Button { act(ObsbotCommand(action: "preset", n: p.n)) } label: {
                    Text(p.name)
                        .font(.subheadline)
                        .lineLimit(1)
                        .foregroundStyle(Color.primary)
                        .padding(.horizontal, 14)
                        .frame(minHeight: 44)
                        .background(Capsule().fill(Color.eveFill))
                }
                .buttonStyle(PressStyle())
                .accessibilityLabel("Go to \(p.name)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // ---- the Mac --------------------------------------------------------------------------------------------------

    private func status() async {
        switch await client.obsbot(ObsbotCommand(action: "status")) {
        case .success(let r):
            if r.ok {
                cam = r
                if !zooming, let z = r.zoom { zoom = Double(z) }
                if line == "" { failed = false }
            } else {
                say(r.line, error: true)
            }
        case .failure(let f):
            say(f.message.isEmpty ? noAnswer : f.message, error: true)
        }
    }

    private func act(_ command: ObsbotCommand, busyLine: String = "") {
        taps += 1
        busy = true
        if !busyLine.isEmpty { say(busyLine, error: false) }
        Task { @MainActor in
            defer { busy = false }
            switch await client.obsbot(command) {
            case .success(let r):
                say(r.line, error: !r.ok)
                await status()
                if !r.line.isEmpty, r.ok {
                    try? await Task.sleep(for: .seconds(2))
                    if line == r.line { line = "" }              // back to where the camera points
                }
            case .failure(let f):
                say(f.message.isEmpty ? noAnswer : f.message, error: true)
            }
        }
    }

    private func say(_ text: String, error: Bool) {
        line = text
        failed = error
        if error { fails += 1 }
    }
}

private let noAnswer = "eve didn't answer. Is the Mac awake?"

/// One arrow: down turns, up stops. A GestureState resets when the system cancels the touch too, so a release is
/// never missed.
private struct TurnKey: View {
    let direction: ObsbotDirection
    let turning: Bool
    let onPress: () -> Void
    let onRelease: () -> Void
    @GestureState private var down = false

    var body: some View {
        Image(systemName: direction.symbol)
            .font(.title2.weight(.semibold))
            .foregroundStyle(turning ? Color.eveBackground : Color.primary)
            .frame(width: 64, height: 64)
            .background(Circle().fill(turning ? Color.primary : Color.eveFill))
            .overlay(Circle().strokeBorder(Color.eveEdge))
            .scaleEffect(down ? 0.94 : 1)
            .animation(.easeOut(duration: 0.1), value: down)
            .contentShape(Circle())
            .gesture(DragGesture(minimumDistance: 0).updating($down) { _, state, _ in state = true })
            .onChange(of: down) { _, now in now ? onPress() : onRelease() }
            .accessibilityElement()
            .accessibilityLabel(direction.label)
            .accessibilityHint("Hold to turn. Lift to stop.")
            .accessibilityAddTraits(.isButton)
    }
}

/// The camera's picture from the Mac, the newest JPEG shown: over WebRTC straight from the Mac (PikaRTC, the
/// camera's own answer) when it connects, else one multipart stream through the tunnel, whose frames come in bursts.
/// Tries again every few seconds while the screen is up; nothing reads the camera while every screen is closed.
@MainActor
final class ObsbotFeed: ObservableObject {
    @Published private(set) var image: UIImage?
    @Published private(set) var problem: String? = "Connecting to the camera…"
    private var session: URLSession?
    private var rtc: PikaRTC?
    private var running = false
    private var retry: Task<Void, Never>?

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
            let ok = await link.connect(client) { await client.obsbotAnswer($0) }
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
        case .frame(let jpeg, _):
            show(jpeg)
        case .line(let line):
            problem = line
        case .closed:
            rtc = nil
            problem = image == nil ? "No picture from the camera." : "The camera's picture stopped."
            again(client)
        }
    }

    private func show(_ jpeg: Data) {
        if let img = UIImage(data: jpeg) {
            image = img
            problem = nil
        }
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
        let receiver = MJPEGReceiver(fallback: "The camera's picture isn't available.") { [weak self] event in
            Task { @MainActor in self?.handle(event, client) }
        }
        let config = URLSessionConfiguration.default
        config.urlCache = nil
        config.httpCookieStorage = nil
        config.timeoutIntervalForRequest = 15
        let s = URLSession(configuration: config, delegate: receiver, delegateQueue: nil)
        session?.invalidateAndCancel()
        session = s
        s.dataTask(with: ControlsRequests.obsbotStream(ep)).resume()
    }

    private func handle(_ event: MJPEGReceiver.Event, _ client: WorkClient) {
        guard running, session != nil else { return }
        switch event {
        case .frame(let data, _):
            show(data)
        case .ended(let line):
            problem = line ?? (image == nil ? "No picture from the camera." : "The camera's picture stopped.")
            session?.finishTasksAndInvalidate()
            session = nil
            again(client)                                  // WebRTC first again: the Wi-Fi may be back
        }
    }
}
