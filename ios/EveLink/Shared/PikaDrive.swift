// Pika, the owner's robot car, in the Apps tab (docs/pika.md): hold-to-drive over its live video, through the Mac
// (pika.py). Foundation only, so Tests/main.swift checks it on the Mac.
//
//   POST /api/link/controls/pika          {"action": "drive", "dir", "hold"} every beat while held,
//                                         {"action": "stop", "hold"} on release or Stop, {"action": "close"} on leaving
//                                         -> {"ok", "line", "stopped"?}
//   GET  /api/link/controls/pika/stream   multipart JPEGs, each part stamped X-Stamp (the Mac's clock, ms), one
//                                         viewer; 503 {"ok": false, "line"} when the camera is off or busy
//   POST /api/link/controls/pika          {"action": "rtc", "sdp": <offer>} -> {"ok", "rtc", "sdp": <answer>}: the
//                                         video over WebRTC (pika_rtc.py), each binary message an 8-byte big-endian
//                                         stamp then the JPEG; a text message is a line. {"ok": false} -> the stream
//   POST /api/link/controls/pika          {"action": "stats", "path": "webrtc"|"tunnel", "n", "p50", "p95", "fps"}
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
    /// The newest returned JPEG's X-Stamp: when it left the Mac, ms since 1970. Nil when its part had none.
    private(set) var stamp: Double?

    private static let stampKey = Data("X-Stamp: ".utf8)

    private static let start = Data([0xFF, 0xD8])
    private static let end = Data([0xFF, 0xD9])

    /// The newest whole JPEG in what arrived so far, or nil.
    mutating func feed(_ data: Data) -> Data? {
        buffer.append(data)
        var newest: Data?
        while let a = buffer.range(of: Self.start),
              let z = buffer.range(of: Self.end, in: a.upperBound..<buffer.endIndex) {
            newest = buffer.subdata(in: a.lowerBound..<z.upperBound)
            stamp = Self.stamp(in: buffer.subdata(in: buffer.startIndex..<a.lowerBound))
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

    /// The last X-Stamp in a part's head.
    static func stamp(in head: Data) -> Double? {
        guard let k = head.range(of: stampKey, options: .backwards) else { return nil }
        let digits = head[k.upperBound...].prefix { $0 >= 0x30 && $0 <= 0x39 }
        return digits.isEmpty ? nil : Double(String(decoding: digits, as: UTF8.self))
    }
}

/// One WebRTC frame message: the Mac's send stamp (8 bytes, big-endian ms) then the JPEG (pika_rtc.frame_message).
enum PikaFrameMessage {
    static func parse(_ data: Data) -> (stamp: Double, jpeg: Data)? {
        guard data.count > 8 else { return nil }
        var ms: UInt64 = 0
        for b in data.prefix(8) { ms = ms << 8 | UInt64(b) }
        return (Double(ms), data.subdata(in: data.startIndex + 8..<data.endIndex))
    }
}

/// How late frames arrive on one video path: the phone's clock minus the Mac's stamp. The two clocks differ by an
/// offset that is the same on both paths, so the paths compare fairly. A summary every `every` seconds of frames.
struct PikaLatency: Sendable {
    let path: String                     // "webrtc" | "tunnel"
    var every: Double = 10
    private(set) var samples: [Double] = []
    private var since: Double?

    init(path: String, every: Double = 10) {
        self.path = path
        self.every = every
    }

    /// A frame stamped `stamp` arrived at `now` (both ms since 1970). Returns the stats body when one is due.
    mutating func add(stamp: Double, now: Double) -> [String: Any]? {
        if since == nil { since = now }
        samples.append(now - stamp)
        guard let start = since, now - start >= every * 1000, samples.count >= 10 else { return nil }
        let sorted = samples.sorted()
        func pct(_ p: Double) -> Double { sorted[min(sorted.count - 1, Int(Double(sorted.count) * p))] }
        let body: [String: Any] = ["action": "stats", "path": path, "n": sorted.count,
                                   "p50": pct(0.5).rounded(), "p95": pct(0.95).rounded(),
                                   "fps": (Double(sorted.count) / ((now - start) / 1000) * 10).rounded() / 10]
        samples = []
        since = now
        return body
    }
}

extension ControlsRequests {
    /// A beat must be quick or useless: the Mac stops the car 450 ms after the last one anyway.
    static func pika(_ ep: ChatEndpoint, _ command: PikaCommand) -> URLRequest {
        var r = WorkRequests.request(ep, ["api", "link", "controls", "pika"], post: true, body: command.body)
        r.timeoutInterval = command.action == "drive" ? 3 : 8
        return r
    }

    /// The WebRTC offer (gathering on the Mac takes up to a few seconds) and the latency stats.
    static func pikaBody(_ ep: ChatEndpoint, _ body: [String: Any], timeout: Double) -> URLRequest {
        var r = WorkRequests.request(ep, ["api", "link", "controls", "pika"], post: true, body: body)
        r.timeoutInterval = timeout
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

    /// The Mac's answer to a WebRTC offer, or nil: then the video comes through the tunnel stream.
    func pikaAnswer(_ offer: String) async -> String? {
        let r = await call(.controls, { ControlsRequests.pikaBody($0, ["action": "rtc", "sdp": offer], timeout: 12) }) {
            data, _ in
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            return (obj?["ok"] as? Bool == true ? obj?["sdp"] as? String : nil) ?? ""
        }
        if case .success(let sdp) = r, !sdp.isEmpty { return sdp }
        return nil
    }

    /// A latency summary (PikaLatency's body, as JSON) for the Mac's log.
    func pikaStats(_ data: Data) async {
        _ = await call(.controls, { ep in
            var r = ControlsRequests.pikaBody(ep, [:], timeout: 5)
            r.httpBody = data
            return r
        }) { _, _ in true }
    }
}
