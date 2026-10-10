// EveService — the menu-bar app's power switch for eve.
//
// eve is the bridge daemon, a launchd agent (`eve-bridge install --service`
// writes ~/Library/LaunchAgents/com.github.eve-bridge.daemon.plist) with
// KeepAlive on, so killing the process only respawns it. Turning eve off means
// taking the agent out of launchd and marking it disabled, so it stays off across
// logins until it is turned back on; on is the reverse. The helper is not
// sandboxed, so it may run /bin/launchctl in the owner's GUI domain.

import Foundation
import os

/// What launchd says about eve's agent.
struct ServiceState: Equatable {
    /// The plist exists: `eve-bridge install --service` ran on this Mac.
    var installed: Bool
    /// The agent is loaded into the owner's GUI domain (launchd will run and keep it).
    var loaded: Bool
    /// The daemon process is up right now (only meaningful when loaded).
    var running: Bool
    var pid: Int?
    /// launchd's persistent override: the owner turned eve off.
    var disabled: Bool
    /// The daemon answers on its socket, whether or not launchd started it.
    var reachable: Bool

    /// eve is on: launchd holds the agent and has not been told to stop.
    var on: Bool { loaded && !disabled }

    /// The daemon is up, but not as the agent: run by hand from a terminal.
    var runByHand: Bool { reachable && !loaded }

    /// One line for the menu.
    var line: String {
        if !installed {
            return runByHand
                ? "eve: running from a terminal (not installed as a service)"
                : "eve: not installed as a service (eve-bridge install --service)"
        }
        if runByHand { return "eve: running from a terminal — stop it there" }
        if loaded {
            if running, let pid { return "eve: on (pid \(pid))" }
            return running ? "eve: on" : "eve: on, starting…"
        }
        if disabled { return "eve: off (your choice) — stays off until you turn it on" }
        return "eve: not running (agent not loaded)"
    }

    /// The button's title, or nil when there is nothing it can do.
    var buttonTitle: String? {
        guard installed, !runByHand else { return nil }
        return on ? "Turn eve off" : "Turn eve on"
    }
}

enum EveService {
    static let label = "com.github.eve-bridge.daemon"
    static let plistPath = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/LaunchAgents/\(label).plist")
    private static let domain = "gui/\(getuid())"
    private static let target = "\(domain)/\(label)"
    private static let log = Logger(subsystem: "com.github.eve-bridge.StackChanNotes", category: "service")
    private static let queue = DispatchQueue(label: "com.github.eve-bridge.StackChanNotes.service")

    /// Ask launchd (and the socket) where eve is.
    static func status() async -> ServiceState {
        let reachable = await EveDaemon.micStatus() != nil
        return await run { statusSync(reachable: reachable) }
    }

    /// Stop eve and keep it stopped: `launchctl bootout` (SIGTERM, a clean
    /// daemon shutdown) then `launchctl disable`, so the next login does not
    /// bring it back. Returns the state once launchd has let the agent go.
    static func turnOff() async -> ServiceState {
        await run {
            let out = launchctl("bootout", target)
            log.info("bootout: \(out.status) \(out.text, privacy: .public)")
            let dis = launchctl("disable", target)
            log.info("disable: \(dis.status) \(dis.text, privacy: .public)")
        }
        // Not loaded AND no longer answering: a daemon still shutting down reads as "running from a
        // terminal", which has no button — the card would lose its switch until the next 30 s tick.
        return await settle { !$0.loaded && !$0.reachable }
    }

    /// Bring eve back: `launchctl enable` clears the override, `launchctl
    /// bootstrap` loads the plist so RunAtLoad starts the daemon now. Returns
    /// the state once the daemon has a pid.
    static func turnOn() async -> ServiceState {
        await run {
            let en = launchctl("enable", target)
            log.info("enable: \(en.status) \(en.text, privacy: .public)")
            let boot = launchctl("bootstrap", domain, plistPath.path)
            log.info("bootstrap: \(boot.status) \(boot.text, privacy: .public)")
        }
        return await settle { $0.running && $0.pid != nil }
    }

    /// `bootout` and `bootstrap` return before launchd is done: right after a
    /// bootout the agent still prints as running, without a pid. Poll until the
    /// state is the one asked for, or give up after ten seconds and show what is.
    private static func settle(until done: (ServiceState) -> Bool) async -> ServiceState {
        var state = await status()
        for _ in 0..<40 where !done(state) {
            try? await Task.sleep(for: .milliseconds(250))
            state = await status()
        }
        return state
    }

    // MARK: - the blocking half, on its own queue

    private static func run<T: Sendable>(_ body: @Sendable @escaping () -> T) async -> T {
        await withCheckedContinuation { cont in
            queue.async { cont.resume(returning: body()) }
        }
    }

    private static func statusSync(reachable: Bool) -> ServiceState {
        let installed = FileManager.default.fileExists(atPath: plistPath.path)
        let print = launchctl("print", target)
        let loaded = print.status == 0
        var running: Bool? = nil
        var pid: Int? = nil
        if loaded {
            // The service's own `state =` and `pid =` come first, at one tab of
            // indent; the endpoints and the process below it repeat `state =`
            // (as "active"), so only the first of each counts.
            for raw in print.text.split(separator: "\n") {
                let line = raw.trimmingCharacters(in: .whitespaces)
                if running == nil, line.hasPrefix("state = ") { running = line.hasSuffix("running") }
                if pid == nil, line.hasPrefix("pid = ") { pid = Int(line.dropFirst("pid = ".count).trimmingCharacters(in: .whitespaces)) }
            }
        }
        // `print-disabled` lists every override in the domain: `"<label>" => disabled`.
        let overrides = launchctl("print-disabled", domain).text
        let disabled = overrides.contains("\"\(label)\" => disabled")
        return ServiceState(installed: installed, loaded: loaded, running: running ?? false, pid: pid,
                            disabled: disabled, reachable: reachable)
    }

    private struct Result {
        var status: Int32
        var text: String
    }

    private static func launchctl(_ args: String...) -> Result {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        proc.arguments = args
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = pipe
        do {
            try proc.run()
        } catch {
            return Result(status: -1, text: error.localizedDescription)
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        proc.waitUntilExit()
        return Result(status: proc.terminationStatus, text: String(decoding: data, as: UTF8.self))
    }
}
