// PowerRelay — the helper's side of the card's power button.
//
// The card (sandboxed) writes power-request.json into the App Group; this
// watches the container directory, serves a request through EveService
// (launchctl), and writes power.json back, then reloads the card so it draws
// the result. It is also the one place the helper asks launchd about eve:
// the menu reads `state` from here, and a 30-second timer keeps power.json
// honest when eve is stopped or started some other way (a terminal, a
// crash, `eve-bridge uninstall --service`).

import Foundation
import WidgetKit
import os

@MainActor
@Observable
final class PowerRelay {
    static let shared = PowerRelay()

    /// launchd's word on eve's agent; nil until asked.
    private(set) var state: ServiceState?
    /// A flip is in progress (the menu disables its button meanwhile).
    private(set) var switching = false

    private let log = Logger(subsystem: "com.github.eve-bridge.StackChanNotes", category: "power")
    private var dirSource: DispatchSourceFileSystemObject?
    private var timer: Timer?
    private var lastWritten: EvePower.State?

    private init() {}

    func start() {
        if let dir = AppGroup.containerURL {
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            dirSource = makeSource(for: dir)
            log.info("watching \(dir.path, privacy: .public) for power requests")
        } else {
            log.error("no App Group container: the card's power button will not work")
        }
        Task { await tick() }
        timer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { _ in
            Task { @MainActor in await PowerRelay.shared.tick() }
        }
    }

    /// Ask launchd where eve is, publish it, and answer any request the card left.
    @discardableResult
    func refresh() async -> ServiceState {
        let now = await EveService.status()
        state = now
        publish(now, force: false)
        return now
    }

    /// Flip eve, from the menu or from the card. Serialised: a second call
    /// while one is running returns the current state untouched.
    @discardableResult
    func turn(on: Bool) async -> ServiceState {
        if switching {
            if let state { return state }
            return await refresh()
        }
        switching = true
        defer { switching = false }
        log.info("turning eve \(on ? "on" : "off", privacy: .public)")
        let now = on ? await EveService.turnOn() : await EveService.turnOff()
        state = now
        publish(now, force: true)
        return now
    }

    private func tick() async {
        await refresh()
        await serve()
    }

    /// Answer the card: a request stamped after the last state we wrote is new.
    private func serve() async {
        guard !switching, let request = EvePower.readRequest() else { return }
        let answered = EvePower.readState()?.at ?? .distantPast
        guard request.at > answered else { return }
        if Date.now.timeIntervalSince(request.at) > EvePower.requestMaxAge, let state {
            // Pressed while this helper was not running. Too old to act on: stamp the state past it
            // so the card stops waiting, and leave eve as it is.
            log.info("ignoring a power request from \(request.at, privacy: .public): too old")
            publish(state, force: true)
            return
        }
        if let state, state.on == request.on {
            // Already there (a stale button, or a double press): stamp the state
            // past the request so the card stops showing it as pending.
            publish(state, force: true)
            return
        }
        await turn(on: request.on)
    }

    /// Write power.json for the card; only on a change unless forced, so the
    /// card is not reloaded every 30 seconds for nothing.
    private func publish(_ s: ServiceState, force: Bool) {
        // Never stamped before the request it answers (a request is stamped a second past the last
        // state, so it can sit up to a second in the future): the card reads "answered" off this.
        let stamp = max(Date.now, EvePower.readRequest()?.at ?? .distantPast)
        let next = EvePower.State(on: s.on, switchable: s.buttonTitle != nil, line: s.line, at: stamp)
        if !force, var last = lastWritten {
            last.at = next.at
            if last == next { return }
        }
        do {
            try EvePower.writeState(next)
            lastWritten = next
            WidgetCenter.shared.reloadAllTimelines()
        } catch {
            log.error("could not write power.json: \(error.localizedDescription, privacy: .public)")
        }
    }

    /// A write on the container directory (a request renamed into place) is a
    /// tick. Rename/delete of the directory itself cancels the source.
    private func makeSource(for url: URL) -> DispatchSourceFileSystemObject? {
        let fd = open(url.path, O_EVTONLY)
        guard fd >= 0 else { return nil }
        let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: [.write, .rename, .delete], queue: .main)
        source.setCancelHandler { close(fd) }
        source.setEventHandler { [weak self, weak source] in
            guard let source else { return }
            if source.data.contains(.rename) || source.data.contains(.delete) { source.cancel() }
            Task { @MainActor in
                guard let self else { return }
                if source.isCancelled, self.dirSource === source { self.dirSource = nil }
                await self.serve()
            }
        }
        source.resume()
        return source
    }
}
