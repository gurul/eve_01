// The rendezvous: eve's current address, found without the owner tapping Update Eve Link. The daemon posts its
// quick tunnel's address to an ntfy.sh topic that only the link token names (bridge/src/eve_bridge/
// stick_link.py, rendezvous_topic). Read at launch and when a call finds the old address gone. 2026-10-04: the
// address changing with every restart was why the first Action Button test "didn't work".
import CryptoKit
import Foundation

enum Rendezvous {
    static let base = URL(string: "https://ntfy.sh")!
    static let label = Data("eve link rendezvous".utf8)

    /// "evelink-" and 32 hex of HMAC-SHA256(token, label): the daemon computes the same.
    static func topic(token: String) -> String {
        let mac = HMAC<SHA256>.authenticationCode(for: label, using: SymmetricKey(data: Data(token.utf8)))
        return "evelink-" + mac.map { String(format: "%02x", $0) }.joined().prefix(32)
    }

    /// The newest address posted on the topic, from ntfy's newline-delimited JSON. Only a bare
    /// https://<name>.trycloudflare.com origin is believed: anything else on the topic is ignored.
    static func newest(in body: String) -> URL? {
        var found: URL?
        for line in body.split(separator: "\n") {
            guard let data = line.data(using: .utf8),
                  let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  obj["event"] as? String == "message",
                  let text = (obj["message"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                  isTunnel(text), let url = URL(string: text) else { continue }
            found = url
        }
        return found
    }

    static func isTunnel(_ text: String) -> Bool {
        text.range(of: #"^https://[a-z0-9-]+\.trycloudflare\.com$"#, options: .regularExpression) != nil
    }

    /// Ask the topic. Nil on any failure: the caller keeps the address it has.
    static func latest(token: String, session: URLSession = .shared) async -> URL? {
        var comps = URLComponents(url: base.appendingPathComponent(topic(token: token)).appendingPathComponent("json"),
                                  resolvingAgainstBaseURL: false)!
        comps.queryItems = [URLQueryItem(name: "poll", value: "1"), URLQueryItem(name: "since", value: "all")]
        var request = URLRequest(url: comps.url!)
        request.timeoutInterval = 8
        guard let (data, response) = try? await session.data(for: request),
              (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
        return newest(in: String(decoding: data, as: UTF8.self))
    }
}

/// Notifications (docs/notifications.md): eve posts what Telegram would have pushed to an ntfy.sh topic, and the free
/// ntfy app shows it. The topic is the daemon's (bridge/src/eve_bridge/notify.py, notify_topic): the
/// rendezvous's construction under its own label, so Settings can show it without the terminal.
enum NotifyTopic {
    static let label = Data("eve notify".utf8)
    static let ntfyApp = URL(string: "ntfy://")!
    static let appStore = URL(string: "https://apps.apple.com/app/ntfy/id1625396347")!

    /// "eve-" and 32 hex of HMAC-SHA256(token, label).
    static func topic(token: String) -> String {
        let mac = HMAC<SHA256>.authenticationCode(for: label, using: SymmetricKey(data: Data(token.utf8)))
        return "eve-" + mac.map { String(format: "%02x", $0) }.joined().prefix(32)
    }
}
