// The chat's rules that need no UI (docs/app-chat.md): how long a message may be, which attachment ids and names are
// well formed, and which links may open. Foundation only, so Tests/main.swift checks all of it on the Mac. The chat's
// own logic (the thread, the ledger, the queue, cards) lives in Packages/ and is tested there.
import Foundation

enum ChatLimits {
    static let maxText = 4000

    /// Over the daemon's limit, counted as the daemon counts (Python's len: Unicode scalars, not Characters), so a
    /// message of emoji, flags or combining marks that passes here is never refused there.
    static func tooLong(_ text: String) -> Bool { text.unicodeScalars.count > maxText }
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

/// The message the composer just sent, held for a moment after it empties the field (owner, 2026-11-09: "sometimes
/// after I send a message it leaves the message in the text box"). The keyboard can put the words back: dictation's
/// last result landing after the tap, or a prediction, autocorrect or composing text committing as the field empties.
/// That comes back whole, in one change; a new message starts with one key, so it never matches.
struct ComposerEcho: Sendable {
    let words: [String]
    let at: Date
    /// How long after a send a change can still be the keyboard putting the words back.
    static let window: TimeInterval = 1.5

    /// nil for a send with no words (attachments only): there is nothing to come back.
    init?(_ sent: String, at: Date = Date()) {
        let words = Self.words(sent)
        guard !words.isEmpty else { return nil }
        self.words = words
        self.at = at
    }

    /// `draft` is the sent message coming back: the same words (case and punctuation aside), the same with the last
    /// word finished or corrected, or the same with more after it.
    func matches(_ draft: String, now: Date = Date()) -> Bool {
        let age = now.timeIntervalSince(at)
        guard age >= 0, age <= Self.window else { return false }
        let back = Self.words(draft)
        guard !back.isEmpty else { return false }
        if back.count > words.count { return back.starts(with: words) }
        if back.count == words.count { return back == words || (words.count >= 2 && back.dropLast() == words.dropLast()) }
        return false
    }

    static func words(_ text: String) -> [String] {
        text.lowercased().split { !$0.isLetter && !$0.isNumber }.map(String.init)
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

/// Where a link in a reply or a card goes: http(s) to Safari in the app; mail, phone, messages, Maps and FaceTime
/// to their apps. Any other scheme is never opened.
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
