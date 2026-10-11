// The OBSBOT webcam in the Apps tab (docs/obsbot.md; owner, 2026-10-11: "build in an app into eve for obsbot"): its
// live picture, hold-to-turn arrows, zoom, center, sleep and wake, tracking and the AI mode, all through the Mac
// (obsbot_panel.py), which drives OBSBOT Center. Foundation only, so Tests/main.swift checks it on the Mac.
//
//   POST /api/link/controls/obsbot          {"action": "status"} -> {"ok", "line", "camera", "awake", "pan", "pitch",
//                                           "zoom", "locked", "presets": [{"n", "set", "name"}], "modes": [...]}
//                                           {"action": "hold", "dir", "hold"} every beat while an arrow is held,
//                                           {"action": "stop", "hold"} on release, {"action": "close"} on leaving
//                                           {"action": "center"|"wake"|"sleep"} | {"action": "zoom", "level"}
//                                           | {"action": "track", "on"} | {"action": "mode", "name"}
//                                           | {"action": "preset", "n"} -> {"ok", "line", ...}
//                                           {"action": "rtc", "sdp": <offer>} -> {"ok", "rtc", "sdp": <answer>}: the
//                                           picture over WebRTC (pika_rtc.py's wire); {"ok": false} -> the stream
//   GET  /api/link/controls/obsbot/stream   multipart JPEGs (Pika's format); 503 {"ok": false, "line"}
import Foundation

enum ObsbotDirection: String, CaseIterable, Sendable {
    case up, down, left, right

    var symbol: String {
        switch self {
        case .up: "chevron.up"
        case .down: "chevron.down"
        case .left: "chevron.left"
        case .right: "chevron.right"
        }
    }

    var label: String { "Turn \(rawValue)" }
}

/// One request to the camera's panel.
struct ObsbotCommand: Equatable, Sendable {
    var action: String
    var dir: ObsbotDirection?
    var hold: String?
    var level: Int?
    var on: Bool?
    var name: String?
    var n: Int?

    var body: [String: Any] {
        var b: [String: Any] = ["action": action]
        if let dir { b["dir"] = dir.rawValue }
        if let hold { b["hold"] = hold }
        if let level { b["level"] = level }
        if let on { b["on"] = on }
        if let name { b["name"] = name }
        if let n { b["n"] = n }
        return b
    }
}

/// A preset saved in OBSBOT Center (1 to 3).
struct ObsbotPreset: Equatable, Sendable, Identifiable {
    var n: Int
    var name: String
    var id: Int { n }
}

/// The panel's answer: a status read fills everything; a command fills what it changed.
struct ObsbotReply: Equatable, Sendable {
    var ok: Bool
    var line: String
    var stopped: Bool = false
    var camera: String?
    var awake: Bool?
    var pan: Int?
    var pitch: Int?
    var zoom: Int?
    var presets: [ObsbotPreset] = []
    var modes: [String] = []

    static func decode(_ data: Data) -> ObsbotReply? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let presets = (obj["presets"] as? [[String: Any]] ?? []).compactMap { p -> ObsbotPreset? in
            guard p["set"] as? Bool == true, let n = p["n"] as? Int else { return nil }
            let name = (p["name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "Preset \(n)"
            return ObsbotPreset(n: n, name: name)
        }
        return ObsbotReply(ok: obj["ok"] as? Bool ?? false, line: obj["line"] as? String ?? "",
                           stopped: obj["stopped"] as? Bool ?? false, camera: obj["camera"] as? String,
                           awake: obj["awake"] as? Bool, pan: obj["pan"] as? Int, pitch: obj["pitch"] as? Int,
                           zoom: obj["zoom"] as? Int, presets: presets,
                           modes: (obj["modes"] as? [String] ?? []).filter { !$0.isEmpty })
    }
}

/// Hold to turn, the way PikaHold drives: one press starts the gimbal, a beat renews it, the release of the turning
/// arrow ends it. Each press has its own id, so the Mac refuses a late beat of a press that already ended.
struct ObsbotHold: Equatable, Sendable {
    /// The Mac stops the gimbal when no beat came for 450 ms (obsbot_panel.HEARTBEAT_SECS).
    static let beat: Duration = .milliseconds(150)

    private(set) var direction: ObsbotDirection?
    private(set) var id: String?

    var holding: Bool { direction != nil }

    mutating func press(_ d: ObsbotDirection, id newID: String) -> ObsbotCommand {
        direction = d
        id = newID
        return ObsbotCommand(action: "hold", dir: d, hold: newID)
    }

    func renew() -> ObsbotCommand? {
        guard let direction, let id else { return nil }
        return ObsbotCommand(action: "hold", dir: direction, hold: id)
    }

    mutating func release(_ d: ObsbotDirection) -> ObsbotCommand? {
        guard direction == d else { return nil }
        return stop()
    }

    mutating func stop() -> ObsbotCommand {
        direction = nil
        return ObsbotCommand(action: "stop", hold: id)
    }

    mutating func ended(_ hold: String) {
        if id == hold { direction = nil }
    }
}

/// The AI modes' names as the owner reads them; the Mac's names are the keys.
enum ObsbotModes {
    static func title(_ name: String) -> String {
        switch name {
        case "single": "Follow one person"
        case "group": "Frame the group"
        case "voice": "Follow the voice"
        case "desk": "Desk"
        case "hand": "Hand"
        case "whiteboard": "Whiteboard"
        default: name.capitalized
        }
    }
}

extension ControlsRequests {
    /// A beat must be quick or useless; a move or a wake reads the camera back and may take a few seconds.
    static func obsbot(_ ep: ChatEndpoint, _ command: ObsbotCommand) -> URLRequest {
        var r = WorkRequests.request(ep, ["api", "link", "controls", "obsbot"], post: true, body: command.body)
        r.timeoutInterval = command.action == "hold" ? 3 : 30
        return r
    }

    static func obsbotStream(_ ep: ChatEndpoint) -> URLRequest {
        var r = WorkRequests.request(ep, ["api", "link", "controls", "obsbot", "stream"], post: false)
        r.setValue("multipart/x-mixed-replace", forHTTPHeaderField: "Accept")
        r.timeoutInterval = 15
        return r
    }
}

extension WorkClient {
    func obsbot(_ command: ObsbotCommand) async -> Result<ObsbotReply, ChatFailure> {
        await call(.controls, { ControlsRequests.obsbot($0, command) }) { data, _ in ObsbotReply.decode(data) }
    }

    /// The Mac's answer to a WebRTC offer for the picture, or nil: then it comes through the tunnel stream.
    func obsbotAnswer(_ offer: String) async -> String? {
        let r = await call(.controls, { ep in
            var req = WorkRequests.request(ep, ["api", "link", "controls", "obsbot"], post: true,
                                           body: ["action": "rtc", "sdp": offer])
            req.timeoutInterval = 12
            return req
        }) { data, _ in
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            return (obj?["ok"] as? Bool == true ? obj?["sdp"] as? String : nil) ?? ""
        }
        if case .success(let sdp) = r, !sdp.isEmpty { return sdp }
        return nil
    }
}
