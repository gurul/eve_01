// EveDaemon — the menu-bar app's one line to the bridge daemon.
//
// The daemon listens on a Unix socket (ipc.py, default /tmp/eve-bridge.sock):
// one JSON object in, one JSON line back. The helper uses it for the owner's
// microphone switch (`{"evt":"mic","action":"on"|"off"|"status"}`), the same
// event `eve-bridge mic` sends. Blocking POSIX sockets on a background
// queue; the app is not sandboxed, so it may open the socket.

import Foundation
import os

/// What the daemon says the Mac microphone is doing, and why.
struct MicState: Equatable {
    /// The owner's switch (persisted by the daemon): true = on.
    var switchOn: Bool
    /// The mic stream is open right now.
    var listening: Bool
    /// The robot is on the cable.
    var robotConnected: Bool
    /// EVE_MIC_ALWAYS=1: open from boot, not only while the robot is connected.
    var always: Bool
    /// The daemon has ears at all (EVE_VOICE on, a mic and the model found).
    var available: Bool

    /// One line for the menu, matching `eve-bridge mic` (cli.describe_mic).
    var line: String {
        if !available { return "Microphone: none (EVE_VOICE=0, or no mic or model)" }
        if !switchOn { return "Microphone: off (your choice)" }
        if listening { return always ? "Microphone: listening (always on)" : "Microphone: listening" }
        if !robotConnected { return "Microphone: closed — opens when the robot connects" }
        return "Microphone: closed — could not open (see the daemon log)"
    }

    init?(response: [String: Any]) {
        guard response["ok"] as? Bool == true, let mic = response["mic"] as? String else { return nil }
        switchOn = mic == "on"
        listening = response["listening"] as? Bool ?? false
        robotConnected = response["connected"] as? Bool ?? false
        always = response["always"] as? Bool ?? false
        available = response["available"] as? Bool ?? true
    }
}

enum EveDaemon {
    static let socketPath = ProcessInfo.processInfo.environment["EVE_BRIDGE_SOCK"] ?? "/tmp/eve-bridge.sock"
    private static let log = Logger(subsystem: "com.github.eve-bridge.StackChanNotes", category: "daemon")
    private static let queue = DispatchQueue(label: "com.github.eve-bridge.StackChanNotes.daemon")

    /// `eve-bridge mic status`. nil when the daemon is not reachable.
    static func micStatus() async -> MicState? { await mic(action: "status") }

    /// `eve-bridge mic on|off`: flip the owner's switch. nil when the daemon is not reachable.
    static func setMic(on: Bool) async -> MicState? { await mic(action: on ? "on" : "off") }

    private static func mic(action: String) async -> MicState? {
        guard let reply = await post(["evt": "mic", "action": action]) else { return nil }
        return MicState(response: reply)
    }

    /// Send one JSON event, read one JSON line, close. nil on any error.
    static func post(_ event: [String: Any], timeoutSeconds: Double = 3.0) async -> [String: Any]? {
        // Only Data crosses the queue: a [String: Any] is not Sendable.
        guard let body = try? JSONSerialization.data(withJSONObject: event) else { return nil }
        let reply: Data? = await withCheckedContinuation { cont in
            queue.async { cont.resume(returning: postSync(body, timeoutSeconds: timeoutSeconds)) }
        }
        guard let reply else { return nil }
        return (try? JSONSerialization.jsonObject(with: reply)) as? [String: Any]
    }

    /// The blocking half: one line out, the first line back as bytes.
    private static func postSync(_ body: Data, timeoutSeconds: Double) -> Data? {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        defer { close(fd) }

        var tv = timeval(tv_sec: Int(timeoutSeconds), tv_usec: Int32((timeoutSeconds - floor(timeoutSeconds)) * 1_000_000))
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(socketPath.utf8CString)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        guard pathBytes.count <= capacity else { return nil }
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            raw.copyBytes(from: pathBytes.map { UInt8(bitPattern: $0) })
        }
        let connected = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                connect(fd, sa, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else {
            log.info("daemon not reachable at \(socketPath, privacy: .public)")
            return nil
        }

        var line = body
        line.append(0x0A)
        let sent = line.withUnsafeBytes { raw in write(fd, raw.baseAddress, raw.count) }
        guard sent == line.count else { return nil }

        var buf = Data()
        var chunk = [UInt8](repeating: 0, count: 4096)
        while true {
            let n = read(fd, &chunk, chunk.count)
            if n <= 0 { break }
            buf.append(contentsOf: chunk[0..<n])
            if buf.contains(0x0A) { break }
        }
        guard let nl = buf.firstIndex(of: 0x0A) else { return nil }
        return Data(buf[buf.startIndex..<nl])
    }
}
