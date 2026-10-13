// The chat page's bridge (docs/app-chat.md): what the bundled OpenUI page may ask the app for, which navigations the
// web view may make, how a value becomes a JavaScript literal, and which bundled file a eveapp:// URL names.
// Foundation only, so Tests/main.swift checks all of it on the Mac.
import Foundation

/// One message the page posts with window.webkit.messageHandlers.eve.postMessage(msg). Anything else is dropped.
enum ChatBridgeMessage: Equatable, Sendable {
    case ready
    /// `attachments`: upload ids (POST /api/link/upload) the turn carries; the text may be empty with one.
    case turn(id: String, text: String, clientID: String, attachments: [String] = [])
    case cancel(id: String)
    case messages(id: String, after: String?, wait: Int)
    case haptic(ChatHaptic)
    case openURL(URL)
    /// Open one attachment full size (a photo or file of the owner's or eve's): native fetches
    /// GET /api/link/file/<id> with the link token and shows Quick Look. `name` is for showing only.
    case openAttachment(id: String, name: String, kind: String)
    /// Horizontal History (docs/app-chat.md): GET /api/link/topics, answered through onLink.
    case topics(id: String)
    /// Save or unsave the topic that starts at message `topic` (POST /api/link/topics/bookmark), answered through onLink.
    case bookmark(id: String, topic: String, on: Bool)
    /// The page's history row opened or closed: while it is open, a sideways swipe scrolls cards, never panes.
    case history(open: Bool)

    static let maxText = 4000
    static let maxID = 128
    static let maxClientID = 64
    static let maxWait = 25

    /// Over the daemon's limit, counted as the daemon counts (Python's len: Unicode scalars, not Characters), so a
    /// message of emoji, flags or combining marks that passes here is never refused there.
    static func tooLong(_ text: String) -> Bool { text.unicodeScalars.count > maxText }

    /// `body` is WKScriptMessage.body: a dictionary for a JS object.
    init?(body: Any) {
        guard let msg = body as? [String: Any], let type = msg["type"] as? String else { return nil }
        func id() -> String? {
            guard let s = msg["id"] as? String, !s.isEmpty, s.count <= Self.maxID else { return nil }
            return s
        }
        switch type {
        case "ready":
            self = .ready
        case "turn":
            guard let id = id(), let raw = msg["text"] as? String,
                  let clientID = msg["client_id"] as? String, !clientID.isEmpty, clientID.count <= Self.maxClientID
            else { return nil }
            let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            var ids: [String] = []
            if let rawIDs = msg["attachments"] {
                guard let list = rawIDs as? [String], ChatAttachments.valid(list) else { return nil }
                ids = list
            }
            guard !text.isEmpty || !ids.isEmpty, !Self.tooLong(text) else { return nil }
            self = .turn(id: id, text: text, clientID: clientID, attachments: ids)
        case "cancel":
            guard let id = id() else { return nil }
            self = .cancel(id: id)
        case "messages":
            guard let id = id() else { return nil }
            let after = (msg["after"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            // JS numbers arrive as NSNumber; a missing or odd wait is no wait.
            let wait = (msg["wait"] as? NSNumber)?.doubleValue ?? 0
            self = .messages(id: id, after: after, wait: wait.isFinite ? min(max(Int(wait), 0), Self.maxWait) : 0)
        case "haptic":
            guard let style = (msg["style"] as? String).flatMap(ChatHaptic.init(rawValue:)) else { return nil }
            self = .haptic(style)
        case "openURL":
            guard let s = msg["url"] as? String, let url = URL(string: s), ChatLinks.external(url) != nil
            else { return nil }
            self = .openURL(url)
        case "openAttachment":
            guard let id = msg["id"] as? String, ChatAttachments.isID(id),
                  let kind = msg["kind"] as? String, ChatFiles.kinds.contains(kind) else { return nil }
            let name = String(((msg["name"] as? String) ?? "").prefix(ChatFiles.maxName))
            self = .openAttachment(id: id, name: name, kind: kind)
        case "history":
            guard let open = msg["open"] as? Bool else { return nil }
            self = .history(open: open)
        case "topics":
            guard let id = id() else { return nil }
            self = .topics(id: id)
        case "bookmark":
            guard let id = id(), let topic = msg["topic"] as? String, ChatTopics.isID(topic),
                  let on = msg["on"] as? Bool else { return nil }
            self = .bookmark(id: id, topic: topic, on: on)
        default:
            return nil
        }
    }
}

/// A Horizontal History topic's id is its first message's: `<day>:<line index>`, as the daemon checks it too.
enum ChatTopics {
    static func isID(_ s: String) -> Bool {
        s.range(of: #"^\d{4}-\d{2}-\d{2}:\d{1,7}$"#, options: .regularExpression) != nil
    }
}

/// Photos and files the composer attaches (docs/app-chat.md, "Attachments"): uploaded first, then named by id in the
/// turn. The daemon checks everything again; this keeps a malformed id from ever reaching a request.
enum ChatAttachments {
    static let maxCount = 4
    /// The daemon's link_uploads.MAX_BYTES.
    static let maxBytes = 20 * 1024 * 1024

    /// 32 lowercase hex characters: the only shape an upload id has.
    static func isID(_ s: String) -> Bool {
        s.utf8.count == 32 && s.utf8.allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102) }
    }

    /// At most `maxCount` ids, each well formed, none twice.
    static func valid(_ ids: [String]) -> Bool {
        ids.count <= maxCount && ids.allSatisfy(isID) && Set(ids).count == ids.count
    }

    /// What window.eveNative.send gets as its second argument: a JSON array of {id, name, kind, thumb?}.
    static func pageJSON(_ items: [ChatUploaded]) -> String {
        let list: [[String: String]] = items.map { u in
            var d = ["id": u.id, "name": u.name, "kind": u.kind]
            if let t = u.thumb { d["thumb"] = t }
            return d
        }
        guard let data = try? JSONSerialization.data(withJSONObject: list) else { return "[]" }
        return String(decoding: data, as: UTF8.self)
    }
}

/// One upload the daemon kept (its answer to POST /api/link/upload), and the thumbnail native made for the bubble.
struct ChatUploaded: Equatable, Sendable {
    var id: String
    var kind: String
    var name: String
    var thumb: String? = nil

    static let kinds: Set<String> = ["image", "pdf", "text", "doc"]

    /// The daemon's JSON → an upload, or nil for anything but an ok answer with a well-formed id and kind.
    static func decode(_ data: Data) -> ChatUploaded? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any], obj["ok"] as? Bool == true,
              let id = obj["id"] as? String, ChatAttachments.isID(id),
              let kind = obj["kind"] as? String, kinds.contains(kind) else { return nil }
        let name = (obj["name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? (kind == "image" ? "Photo" : "File")
        return ChatUploaded(id: id, kind: kind, name: name)
    }
}

/// Opening an attachment full size (docs/app-chat.md, "eve's pictures and files"): which kinds there are, and the
/// local file name it is shown under. The name is the owner's or eve's, so it is cleaned to one safe path component;
/// the bytes are written under a folder named by the id, never a path from the name.
enum ChatFiles {
    /// link_uploads.KINDS: an upload's kinds, and "file" for eve's own anything-else.
    static let kinds: Set<String> = ["image", "pdf", "text", "doc", "file"]
    static let maxName = 200

    /// The daemon's Content-Type → an extension, for a name that has none (Quick Look picks its viewer by it).
    static let extensions: [String: String] = [
        "image/jpeg": "jpg", "image/png": "png", "image/gif": "gif", "image/webp": "webp", "application/pdf": "pdf",
        "text/plain": "txt", "text/markdown": "md", "text/csv": "csv", "text/tab-separated-values": "tsv",
        "application/json": "json", "application/yaml": "yaml", "application/rtf": "rtf", "application/zip": "zip",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document": "docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet": "xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation": "pptx",
    ]

    /// One path component: no slash, colon, control or format character, no leading dot, at most 120 characters (the
    /// extension kept), with the type's extension added when it has none. Empty: "Photo" or "File".
    static func safeName(_ raw: String, kind: String, mime: String) -> String {
        var name = String(String.UnicodeScalarView(raw.unicodeScalars.filter { s in
            !(s == "/" || s == ":" || s == "\\" || s.properties.generalCategory == .control
              || s.properties.generalCategory == .format)
        })).trimmingCharacters(in: .whitespacesAndNewlines)
        while name.hasPrefix(".") { name.removeFirst() }
        if name.isEmpty { name = kind == "image" ? "Photo" : "File" }
        let base = mime.split(separator: ";").first.map { $0.trimmingCharacters(in: .whitespaces).lowercased() } ?? ""
        if (name as NSString).pathExtension.isEmpty, let add = extensions[base] {
            name += "." + add
        }
        if name.count > 120 {
            let e = (name as NSString).pathExtension
            let stem = (name as NSString).deletingPathExtension
            name = e.isEmpty || e.count > 8 ? String(name.prefix(120)) : String(stem.prefix(119 - e.count)) + "." + e
        }
        return name
    }
}

enum ChatHaptic: String, Sendable {
    case light, soft, success
}

/// Where a link the page wants to open goes: http(s) to Safari in the app; mail, phone, messages, Maps and FaceTime
/// to their apps. The page's links.ts allows the same schemes.
enum ChatLinks {
    enum Opener: Equatable { case safari, system }
    static func external(_ url: URL) -> Opener? {
        switch url.scheme?.lowercased() {
        case "http", "https": return url.host?.isEmpty == false ? .safari : nil
        case "mailto", "tel", "sms", "maps", "facetime", "facetime-audio": return .system
        default: return nil
        }
    }
}

/// The web view only ever shows the bundled page. Every other navigation is cancelled; a link the owner tapped is
/// opened outside the web view instead.
enum ChatNavigation: Equatable {
    case allow
    case refuse
    case open(URL)

    static let scheme = "eveapp"
    static let host = "chat"
    static let start = URL(string: "eveapp://chat/index.html")!

    static func decide(_ url: URL?, linkActivated: Bool) -> ChatNavigation {
        guard let url else { return .refuse }
        if url.scheme?.lowercased() == scheme, url.host?.lowercased() == host { return .allow }
        if linkActivated, ChatLinks.external(url) != nil { return .open(url) }
        return .refuse
    }
}

/// Values written into JavaScript source. Strings become double-quoted literals with every character that could end
/// or break the literal escaped, so text from eve can never run as code.
enum ChatJS {
    static func literal(_ s: String) -> String {
        var out = "\""
        out.reserveCapacity(s.utf8.count + 2)
        for scalar in s.unicodeScalars {
            switch scalar {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            case "<": out += "\\u003c"              // harmless here, but keeps "</script>" out of any page source
            case "\u{2028}": out += "\\u2028"
            case "\u{2029}": out += "\\u2029"
            default:
                if scalar.value < 0x20 || scalar.value == 0x7f {
                    out += String(format: "\\u%04x", scalar.value)
                } else {
                    out.unicodeScalars.append(scalar)
                }
            }
        }
        return out + "\""
    }

    static func literal(_ b: Bool) -> String { b ? "true" : "false" }

    /// `{top:…,bottom:…}` for window.eveNative.setInsets, in CSS px (points), one decimal; anything not finite or
    /// below zero is 0. Numbers only, so nothing in it can run as code.
    static func insets(top: Double, bottom: Double) -> String {
        func px(_ v: Double) -> String { String(format: "%.1f", v.isFinite ? max(0, v) : 0) }
        return "{top:\(px(top)),bottom:\(px(bottom))}"
    }

    /// `window.eveNative.<fn>(args)`, skipped quietly when the page has not set that function up (yet).
    static func call(_ fn: String, _ args: [String] = []) -> String {
        "(function(){var n=window.eveNative;if(n&&typeof n.\(fn)==='function'){n.\(fn)(\(args.joined(separator: ",")));}})();void 0"
    }
}

/// The bundled page's files (App/ChatWeb), served by the eveapp:// scheme handler.
enum ChatAssets {
    /// The file under `root` that `url` names, or nil: another scheme or host, a path that climbs out of the root,
    /// or no such file.
    static func resolve(_ url: URL, root: URL, exists: (URL) -> Bool = { FileManager.default.fileExists(atPath: $0.path) })
        -> URL? {
        guard url.scheme?.lowercased() == ChatNavigation.scheme, url.host?.lowercased() == ChatNavigation.host
        else { return nil }
        var path = url.path
        if path.isEmpty || path == "/" { path = "/index.html" }
        let parts = path.split(separator: "/", omittingEmptySubsequences: true)
        guard !parts.isEmpty, !parts.contains(where: { $0 == ".." || $0 == "." || $0.hasPrefix(".") }) else { return nil }
        var file = root
        for p in parts { file.appendPathComponent(String(p)) }
        let base = root.standardizedFileURL.path
        guard file.standardizedFileURL.path.hasPrefix(base + "/"), exists(file) else { return nil }
        return file
    }

    static func mimeType(_ file: URL) -> String {
        switch file.pathExtension.lowercased() {
        case "html", "htm": return "text/html; charset=utf-8"
        case "js", "mjs": return "text/javascript; charset=utf-8"
        case "css": return "text/css; charset=utf-8"
        case "json", "map": return "application/json; charset=utf-8"
        case "svg": return "image/svg+xml"
        case "png": return "image/png"
        case "jpg", "jpeg": return "image/jpeg"
        case "gif": return "image/gif"
        case "webp": return "image/webp"
        case "woff2": return "font/woff2"
        case "woff": return "font/woff"
        case "ttf": return "font/ttf"
        case "wasm": return "application/wasm"
        case "txt": return "text/plain; charset=utf-8"
        default: return "application/octet-stream"
        }
    }

    /// The page may load its own files, data: and blob: URLs, and https images; it may not fetch anything itself
    /// (the app's native code does all networking).
    static let contentSecurityPolicy = [
        "default-src eveapp: data: blob: 'unsafe-inline'",
        "img-src eveapp: data: blob: https:",
        "connect-src 'none'",
        "base-uri 'none'",
        "form-action 'none'",
    ].joined(separator: "; ")

    static func headers(for file: URL, length: Int) -> [String: String] {
        [
            "Content-Type": mimeType(file),
            "Content-Length": String(length),
            "Cache-Control": "no-store",
            "Access-Control-Allow-Origin": "*",   // module scripts load with CORS, even from their own origin
            "Content-Security-Policy": contentSecurityPolicy,
        ]
    }
}
