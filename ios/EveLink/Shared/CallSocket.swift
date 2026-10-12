// The phone's end of the call: the daemon's /api/stick WebSocket (bridge/src/eve_bridge/stick_link.py and
// phone_call.py), through the Mini App's tunnel.
//
// The wire is the Mini App's "Call eve": 24 kHz mono 16-bit PCM in binary frames both ways; JSON text frames
// {"type": "talk" | "done" | "end" | "ping"} up, {"type": "state" | "heard" | "said" | "expression" | "flush" | "ended"} down ("said": a sentence of eve's, just before
// its sound, for voice mode's captions; "expression": a label for voice mode's face, Shared/FaceExpression.swift). The first
// message proves the owner: here {"token": …, "can": ["notes", "eyes"]}, the link token /stick made, and what this build can
// do. "eyes" says it can draw eve's expressions, so the daemon sends them. "notes" says it can take notes on the phone (docs/action-button.md, Notes on the go): the daemon then sends
// {"type": "state", "state": "notes", "started": <unix s>, "words": <n>}, the phone streams its microphone with no
// talk or done, and {"type": "notes_stop"} stops the notes. While notes are on eve answers each ping with
// {"type": "pong"}: a notes call that hears nothing for NotesLiveness.deadAfter is closed as a drop, and Relay dials
// back.
import Foundation

enum CallEvent {
    case open
    case state(String, note: String?)
    case notes(started: Date?, words: Int)     // notes on the phone are on (sent again after a reply or a call back)
    case heard(String)
    case said(String)              // a sentence of eve's, sent just before its sound (voice mode's captions)
    case expression(FaceExpressionRequest)   // eve's expression for voice mode's face (Shared/FaceExpression.swift)
    case audio([Int16])
    case flush
    case ended(String)
    case failed(String)
}

@MainActor
final class CallSocket {
    var onEvent: ((CallEvent) -> Void)?
    private var task: URLSessionWebSocketTask?
    private var opened = false
    private var waiting: [URLSessionWebSocketTask.Message] = []   // sent before the call was up
    private var pinger: Task<Void, Never>?
    private var liveness = NotesLiveness()
    private let session = URLSession(configuration: .default)

    var isOpen: Bool { task != nil }
    /// The last call ended by eve's own `ended` message, not by a dropped connection. A drop while notes are on is
    /// dialled back (Relay); an end eve chose is not.
    private(set) var endedByEve = false

    func open(base: URL, token: String) {
        guard task == nil else { return }
        var comps = URLComponents(url: base, resolvingAgainstBaseURL: false)!
        comps.scheme = comps.scheme == "http" ? "ws" : "wss"
        comps.path = "/api/stick"
        let t = session.webSocketTask(with: comps.url!)
        task = t
        opened = false
        t.resume()
        endedByEve = false
        liveness = NotesLiveness()
        t.send(.string(Self.json(["token": token, "can": ["notes", "eyes"]]))) { _ in }
        receive(t)
        pinger = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(10))     // the daemon ends a call quiet for 30 s
                guard let self, self.task === t else { return }
                if self.liveness.dead() {
                    // Notes are on and not even a pong came back: the connection is half-open. Close it as a drop,
                    // so Relay holds the microphone's audio and calls back instead of streaming into nothing.
                    self.close(t, reason: "eve stopped answering.")
                    return
                }
                self.sendNow(.string(Self.json(["type": "ping"])))
            }
        }
    }

    func talk() { send(.string(Self.json(["type": "talk"]))) }
    func done() { send(.string(Self.json(["type": "done"]))) }
    func notesStop() { send(.string(Self.json(["type": "notes_stop"]))) }

    func audio(_ pcm: [Int16]) {
        let data = pcm.withUnsafeBufferPointer { Data(buffer: $0) }   // little-endian on every Apple chip
        send(.data(data))
    }

    func hangUp() {
        guard let t = task else { return }
        t.send(.string(Self.json(["type": "end"]))) { _ in }
        close(t, reason: nil)
    }

    /// Sent now, or queued while the call is opening. With no call at all it is dropped: a queue kept across a drop
    /// went out first on the next call, ahead of what that call should send first (review, 2026-10-04: a second Stop
    /// pressed while dialling back reached eve before the held notes audio, which then missed the notes).
    private func send(_ m: URLSessionWebSocketTask.Message) {
        if opened { sendNow(m) } else if task != nil { waiting.append(m) }
    }

    private func sendNow(_ m: URLSessionWebSocketTask.Message) {
        guard let t = task else { return }
        t.send(m) { [weak self] error in
            guard let error else { return }
            Task { @MainActor in self?.close(t, reason: "The connection to eve dropped (\(error.localizedDescription)).") }
        }
    }

    private func receive(_ t: URLSessionWebSocketTask) {
        t.receive { [weak self] result in
            Task { @MainActor in
                guard let self, self.task === t else { return }
                switch result {
                case .failure(let error):
                    self.close(t, reason: Self.explain(error, t))
                case .success(let message):
                    self.handle(message)
                    if self.task === t { self.receive(t) }
                }
            }
        }
    }

    private func handle(_ message: URLSessionWebSocketTask.Message) {
        liveness.heard()
        switch message {
        case .data(let data):
            guard let pcm = PCM16.decode(data) else { return }
            onEvent?(.audio(pcm))
        case .string(let text):
            guard let obj = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
                  let type = obj["type"] as? String else { return }
            switch type {
            case "state":
                let state = obj["state"] as? String ?? ""
                if state == "connected", !opened {
                    opened = true
                    let queued = waiting
                    waiting.removeAll()
                    queued.forEach(sendNow)
                    onEvent?(.open)
                } else if state == "notes" {
                    liveness.watch(true)
                    let started = (obj["started"] as? Double).map { Date(timeIntervalSince1970: $0) }
                    onEvent?(.notes(started: started, words: obj["words"] as? Int ?? 0))
                } else {
                    liveness.watch(false)
                    onEvent?(.state(state, note: obj["note"] as? String))
                }
            case "heard": onEvent?(.heard(obj["text"] as? String ?? ""))
            case "said": onEvent?(.said(obj["text"] as? String ?? ""))
            case "expression":
                if let request = FaceExpressionRequest.decode(obj) { onEvent?(.expression(request)) }
            case "flush": onEvent?(.flush)
            case "ended":
                if let t = task {
                    endedByEve = true
                    close(t, reason: obj["reason"] as? String ?? "The call ended.")
                }
            default: break
            }
        @unknown default: break
        }
    }

    private func close(_ t: URLSessionWebSocketTask, reason: String?) {
        guard task === t else { return }
        t.cancel(with: .normalClosure, reason: nil)
        task = nil
        opened = false
        waiting.removeAll()
        pinger?.cancel()
        pinger = nil
        if let reason { onEvent?(.ended(reason)) }
    }

    private static func explain(_ error: Error, _ t: URLSessionWebSocketTask) -> String {
        if let http = t.response as? HTTPURLResponse {
            switch http.statusCode {
            case 404, 530, 502: return "eve's address changed and the app couldn't find the new one. Is the Mac awake?"
            case 403: return "eve refused the app. Pair again: run eve-bridge link pair on the Mac."
            default: return "eve answered \(http.statusCode)."
            }
        }
        let ns = error as NSError
        if ns.domain == NSURLErrorDomain, [NSURLErrorCannotFindHost, NSURLErrorDNSLookupFailed].contains(ns.code) {
            return "eve's address changed and the app couldn't find the new one. Is the Mac awake?"
        }
        if ns.domain == NSURLErrorDomain, ns.code == NSURLErrorNotConnectedToInternet {
            return "The phone is offline."
        }
        return "Couldn't reach eve (\(error.localizedDescription))."
    }

    private static func json(_ obj: [String: Any]) -> String {
        String(decoding: (try? JSONSerialization.data(withJSONObject: obj)) ?? Data(), as: UTF8.self)
    }
}
