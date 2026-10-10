// Pika, the owner's robot car, in the Apps tab (docs/pika.md): hold-to-drive over its live video, through the Mac
// (pika.py). Foundation only, so Tests/main.swift checks it on the Mac.
//
//   POST /api/link/controls/pika          {"action": "drive", "dir", "hold"} every beat while held,
//                                         {"action": "stop", "hold"} on release or Stop, {"action": "close"} on leaving
//                                         -> {"ok", "line", "stopped"?}
//   GET  /api/link/controls/pika/stream   multipart JPEGs, one viewer; 503 {"ok": false, "line"} when the camera is
//                                         off or busy
import Foundation

enum PikaDirection: String, CaseIterable, Sendable {
    case forward, back, left, right

    var symbol: String {
        switch self {
        case .forward: "chevron.up"
        case .back: "chevron.down"
        case .left: "chevron.left"
        case .right: "chevron.right"
        }
    }

    var label: String {
        switch self {
        case .forward: "Forward"
        case .back: "Back"
        case .left: "Left"
        case .right: "Right"
        }
    }
}

/// One request to the car's panel.
struct PikaCommand: Equatable, Sendable {
    var action: String          // drive | stop | close
    var dir: PikaDirection?
    var hold: String?

    var body: [String: Any] {
        var b: [String: Any] = ["action": action]
        if let dir { b["dir"] = dir.rawValue }
        if let hold { b["hold"] = hold }
        return b
    }
}

/// The panel's answer. `stopped`: the Mac ended this press (late beat, lost car, busy car); lift and press again.
struct PikaReply: Equatable, Sendable {
    var ok: Bool
    var stopped: Bool
    var line: String

    static func decode(_ data: Data) -> PikaReply? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        return PikaReply(ok: obj["ok"] as? Bool ?? false, stopped: obj["stopped"] as? Bool ?? false,
                         line: (obj["line"] as? String) ?? "")
    }
}

/// Hold to drive: one press drives, a beat renews it, any release of the driving key or Stop ends it. Each press has
/// its own id, so the Mac refuses a late beat of a press that already ended.
struct PikaHold: Equatable, Sendable {
    /// How often a held key beats. The Mac stops the car when no beat came for 450 ms (pika.HEARTBEAT_SECS).
    static let beat: Duration = .milliseconds(150)

    private(set) var direction: PikaDirection?
    private(set) var id: String?

    var holding: Bool { direction != nil }

    /// A key went down: it drives now, replacing any other.
    mutating func press(_ d: PikaDirection, id newID: String) -> PikaCommand {
        direction = d
        id = newID
        return PikaCommand(action: "drive", dir: d, hold: newID)
    }

    /// The beat while a key is held; nil when nothing is.
    func renew() -> PikaCommand? {
        guard let direction, let id else { return nil }
        return PikaCommand(action: "drive", dir: direction, hold: id)
    }

    /// A key came up: a stop when it was the driving one. A key a newer press replaced says nothing.
    mutating func release(_ d: PikaDirection) -> PikaCommand? {
        guard direction == d else { return nil }
        return stop()
    }

    /// Stop, always: the button, leaving the screen, the app going to the background.
    mutating func stop() -> PikaCommand {
        direction = nil
        return PikaCommand(action: "stop", dir: nil, hold: id)
    }

    /// The Mac said the press `hold` is over: stop beating it.
    mutating func ended(_ hold: String) {
        if id == hold { direction = nil }
    }
}

/// Cuts JPEGs out of the multipart stream between the start and end markers (as pika.split_jpegs does), keeping only
/// the newest whole one and at most `cap` bytes of a part still coming.
struct MJPEGSplitter: Sendable {
    private(set) var buffer = Data()
    var cap = 1 << 20

    private static let start = Data([0xFF, 0xD8])
    private static let end = Data([0xFF, 0xD9])

    /// The newest whole JPEG in what arrived so far, or nil.
    mutating func feed(_ data: Data) -> Data? {
        buffer.append(data)
        var newest: Data?
        while let a = buffer.range(of: Self.start),
              let z = buffer.range(of: Self.end, in: a.upperBound..<buffer.endIndex) {
            newest = buffer.subdata(in: a.lowerBound..<z.upperBound)
            buffer = buffer.subdata(in: z.upperBound..<buffer.endIndex)
        }
        if let a = buffer.range(of: Self.start) {
            buffer = buffer.subdata(in: a.lowerBound..<buffer.endIndex)
        } else {
            buffer = Data(buffer.suffix(1))
        }
        if buffer.count > cap { buffer = Data() }
        return newest
    }
}

extension ControlsRequests {
    /// A beat must be quick or useless: the Mac stops the car 450 ms after the last one anyway.
    static func pika(_ ep: ChatEndpoint, _ command: PikaCommand) -> URLRequest {
        var r = WorkRequests.request(ep, ["api", "link", "controls", "pika"], post: true, body: command.body)
        r.timeoutInterval = command.action == "drive" ? 3 : 8
        return r
    }

    static func pikaStream(_ ep: ChatEndpoint) -> URLRequest {
        var r = WorkRequests.request(ep, ["api", "link", "controls", "pika", "stream"], post: false)
        r.setValue("multipart/x-mixed-replace", forHTTPHeaderField: "Accept")
        r.timeoutInterval = 15
        return r
    }
}

extension WorkClient {
    func pika(_ command: PikaCommand) async -> Result<PikaReply, ChatFailure> {
        await call(.controls, { ControlsRequests.pika($0, command) }) { data, _ in PikaReply.decode(data) }
    }
}
