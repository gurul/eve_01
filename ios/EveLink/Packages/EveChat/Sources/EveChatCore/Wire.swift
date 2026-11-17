// What the Mac sends the chat (docs/app-chat.md): one answer of GET /api/link/messages, read leniently. A row that is
// malformed is dropped, never shown; a field that is malformed is left out. Attachments, sources and reactions are
// cleaned here, once, so nothing downstream handles a value the daemon did not mean to send.
import Foundation

/// One transcript line as the app's message (link_chat._row).
public struct WireMessage: Equatable, Sendable {
    public enum Role: String, Sendable { case owner, eve }

    public var id: String
    public var role: Role
    public var text: String
    /// When it was said, seconds since 1970.
    public var t: Double
    /// "say", "tool", "image" or "media". A "tool" line is never shown.
    public var kind: String
    /// An owner line's photos and files, or the picture or file eve sent (a "media" line).
    public var attachments: [ChatAttachment]
    /// An owner line's app message id, which its reaction is keyed by.
    public var rid: String?
    /// eve's reaction on an owner line, when the row carries its own (an older daemon).
    public var reaction: String
    /// The memory lines an eve reply leaned on.
    public var sources: [ChatSource]

    public init(id: String, role: Role, text: String, t: Double = 0, kind: String = "say",
                attachments: [ChatAttachment] = [], rid: String? = nil, reaction: String = "",
                sources: [ChatSource] = []) {
        self.id = id
        self.role = role
        self.text = text
        self.t = t
        self.kind = kind
        self.attachments = attachments
        self.rid = rid
        self.reaction = reaction
        self.sources = sources
    }

    /// A row of the daemon's JSON, or nil for anything that is not a message with an id, a role and text.
    public init?(json: Any) {
        guard let row = json as? [String: Any],
              let id = row["id"] as? String, !id.isEmpty,
              let role = (row["role"] as? String).flatMap(Role.init(rawValue:)),
              let text = row["text"] as? String else { return nil }
        self.id = id
        self.role = role
        self.text = text
        self.t = (row["t"] as? NSNumber)?.doubleValue ?? 0
        self.kind = (row["kind"] as? String) ?? "say"
        var attachments = ChatAttachment.sanitize(row["attachments"])
        if role == .owner {
            // An attachment whose upload expired shows as its name, without the picture.
            attachments = attachments.map { a in
                var a = a
                if a.gone { a.thumb = nil }
                return a
            }
        }
        self.attachments = attachments
        self.rid = role == .owner ? (row["rid"] as? String).flatMap { ChatReactions.isRid($0) ? $0 : nil } : nil
        self.reaction = role == .owner ? ChatReactions.sanitize(row["reaction"]) : ""
        self.sources = role == .eve ? ChatSource.sanitize(row["sources"]) : []
    }

    /// Whether the chat draws this line: an owner's or eve's words, or a picture or file with no words.
    public var visible: Bool {
        if kind == "tool" { return false }
        return !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty
    }
}

/// One answer of GET /api/link/messages.
public struct MessagesPayload: Equatable, Sendable {
    public var messages: [WireMessage]
    public var cursor: String
    public var busy: Bool
    /// Every reaction the daemon keeps, by rid; nil from a daemon that sends none.
    public var reactions: [String: String]?
    /// An older page only: whether still older messages exist.
    public var more: Bool?

    public init(messages: [WireMessage] = [], cursor: String = "", busy: Bool = false,
                reactions: [String: String]? = nil, more: Bool? = nil) {
        self.messages = messages
        self.cursor = cursor
        self.busy = busy
        self.reactions = reactions
        self.more = more
    }

    /// The daemon's JSON text, or nil when it is not an object with a `messages` list.
    public static func parse(_ json: String) -> MessagesPayload? {
        guard let raw = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any],
              let rows = raw["messages"] as? [Any] else { return nil }
        return MessagesPayload(
            messages: rows.compactMap(WireMessage.init(json:)),
            cursor: (raw["cursor"] as? String) ?? "",
            busy: (raw["busy"] as? Bool) == true,
            reactions: ChatReactions.cleanSet(raw["reactions"]),
            more: raw["more"] as? Bool)
    }
}

/// A photo or file on a message: the owner's upload, or eve's own picture or file.
public struct ChatAttachment: Equatable, Hashable, Sendable, Identifiable {
    /// "file" is eve's own only: anything the upload door would not take (a zip).
    public enum Kind: String, Sendable, CaseIterable { case image, pdf, text, doc, file }

    /// The upload's id: 32 lowercase hex characters.
    public var id: String
    public var name: String
    public var kind: Kind
    /// A small JPEG or PNG as a data: URL (images only), while the upload lives.
    public var thumb: String?
    /// The upload expired (kept 24 h): shown by name, with nothing to open.
    public var gone: Bool

    public init(id: String, name: String, kind: Kind, thumb: String? = nil, gone: Bool = false) {
        self.id = id
        self.name = name
        self.kind = kind
        self.thumb = thumb
        self.gone = gone
    }

    public static let maxCount = 4
    static let maxThumbChars = 400_000

    public static func isID(_ s: String) -> Bool {
        s.utf8.count == 32 && s.utf8.allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102) }
    }

    /// A data: URL of a JPEG or PNG and nothing else: never a URL the app would fetch.
    public static func isThumb(_ s: String) -> Bool {
        guard s.utf8.count <= maxThumbChars else { return false }
        for prefix in ["data:image/jpeg;base64,", "data:image/png;base64,"] where s.hasPrefix(prefix) {
            let body = s.utf8.dropFirst(prefix.utf8.count)
            guard !body.isEmpty else { return false }
            var padding = 0
            for byte in body {
                if byte == UInt8(ascii: "=") {
                    padding += 1
                    continue
                }
                let ok = (byte >= 48 && byte <= 57) || (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122)
                    || byte == UInt8(ascii: "+") || byte == UInt8(ascii: "/")
                if !ok || padding > 0 { return false }
            }
            return padding <= 2 && body.count > padding
        }
        return false
    }

    /// The thumbnail's bytes, for drawing.
    public var thumbData: Data? {
        guard let thumb, let comma = thumb.firstIndex(of: ",") else { return nil }
        return Data(base64Encoded: String(thumb[thumb.index(after: comma)...]))
    }

    /// Well-formed attachments only, at most `maxCount`.
    public static func sanitize(_ raw: Any?) -> [ChatAttachment] {
        guard let list = raw as? [Any] else { return [] }
        var out: [ChatAttachment] = []
        for item in list {
            if out.count >= maxCount { break }
            guard let a = item as? [String: Any],
                  let id = a["id"] as? String, isID(id),
                  let kind = (a["kind"] as? String).flatMap(Kind.init(rawValue:)) else { continue }
            let given = (a["name"] as? String) ?? ""
            let name = given.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? (kind == .image ? "Photo" : "File") : String(given.prefix(120))
            var att = ChatAttachment(id: id, name: name, kind: kind)
            if (a["gone"] as? Bool) == true {
                att.gone = true
            } else if kind == .image, let thumb = a["thumb"] as? String, isThumb(thumb) {
                att.thumb = thumb
            }
            out.append(att)
        }
        return out
    }
}

/// One memory line a reply leaned on (docs/app-chat.md, "Sources").
public struct ChatSource: Equatable, Hashable, Sendable {
    public enum Kind: String, Sendable {
        case fact, remembered, said

        /// The word shown for the kind.
        public var label: String {
            switch self {
            case .fact: "Fact"
            case .remembered: "Remembered"
            case .said: "Said"
            }
        }
    }

    public var kind: Kind
    public var text: String
    public var when: String?

    public init(kind: Kind, text: String, when: String? = nil) {
        self.kind = kind
        self.text = text
        self.when = when
    }

    public static let maxCount = 6
    public static let maxChars = 200
    public static let maxWhenChars = 40

    /// One line, at most `cap` characters, cut with an ellipsis.
    static func clip(_ raw: String, _ cap: Int) -> String {
        let s = raw.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        guard s.count > cap else { return s }
        var cut = String(s.prefix(cap - 1))
        while cut.last?.isWhitespace == true { cut.removeLast() }
        return cut + "…"
    }

    /// Well-formed sources only, at most `maxCount`, no repeats.
    public static func sanitize(_ raw: Any?) -> [ChatSource] {
        guard let list = raw as? [Any] else { return [] }
        return merged(list.compactMap { item -> ChatSource? in
            guard let s = item as? [String: Any],
                  let kind = (s["kind"] as? String).flatMap(Kind.init(rawValue:)),
                  let text = s["text"] as? String else { return nil }
            return ChatSource(kind: kind, text: text, when: s["when"] as? String)
        })
    }

    /// `sources` clipped, without repeats, at most `maxCount`: what one bubble shows.
    public static func merged(_ sources: [ChatSource]) -> [ChatSource] {
        var out: [ChatSource] = []
        var seen = Set<String>()
        for s in sources {
            if out.count >= maxCount { break }
            let body = clip(s.text, maxChars)
            let key = "\(s.kind.rawValue)\u{0}\(body)"
            if body.isEmpty || seen.contains(key) { continue }
            seen.insert(key)
            let when = s.when.map { clip($0, maxWhenChars) }.flatMap { $0.isEmpty ? nil : $0 }
            out.append(ChatSource(kind: s.kind, text: body, when: when))
        }
        return out
    }
}

/// eve's reaction on the owner's messages, as in Telegram (docs/app-chat.md, "Reactions").
public enum ChatReactions {
    /// A reaction is one emoji; a joined sequence with modifiers fits in this many UTF-16 units.
    public static let maxChars = 16
    static let maxKept = 400

    /// The daemon's key for an app message: 8 to 64 of A-Z a-z 0-9 _ -.
    public static func isRid(_ s: String) -> Bool {
        let n = s.utf8.count
        guard n >= 8, n <= 64 else { return false }
        return s.utf8.allSatisfy { b in
            (b >= 48 && b <= 57) || (b >= 65 && b <= 90) || (b >= 97 && b <= 122) || b == UInt8(ascii: "_")
                || b == UInt8(ascii: "-")
        }
    }

    /// `raw` when it is a short emoji string, else "". Pictographs, skin tones, the variation selector, the keycap
    /// mark and the joiner only: no letters, digits, spaces or markup ever reach the badge.
    public static func sanitize(_ raw: Any?) -> String {
        guard let s = raw as? String, !s.isEmpty, s.utf16.count <= maxChars else { return "" }
        var pictograph = false
        for scalar in s.unicodeScalars {
            let p = scalar.properties
            if isPictographic(scalar) {
                pictograph = true
            } else if !(p.isEmojiModifier || scalar.value == 0xFE0F || scalar.value == 0x20E3 || scalar.value == 0x200D) {
                return ""
            }
        }
        return pictograph ? s : ""
    }

    /// Unicode's Extended_Pictographic, without the ASCII digits, # and * (which are Emoji but not pictographs).
    static func isPictographic(_ scalar: Unicode.Scalar) -> Bool {
        let p = scalar.properties
        // Regional indicators (flag halves) and skin tones are Emoji, but not pictographs.
        if (0x1F1E6...0x1F1FF).contains(scalar.value) || p.isEmojiModifier { return false }
        return p.isEmoji && scalar.value > 0xFF
    }

    /// The daemon's `reactions` object, cleaned; nil when the answer carries none.
    static func cleanSet(_ raw: Any?) -> [String: String]? {
        guard let set = raw as? [String: Any] else { return nil }
        var out: [String: String] = [:]
        for (rid, emoji) in set.sorted(by: { $0.key < $1.key }) {
            if out.count >= maxKept { break }
            let clean = sanitize(emoji)
            if isRid(rid), !clean.isEmpty { out[rid] = clean }
        }
        return out
    }
}
