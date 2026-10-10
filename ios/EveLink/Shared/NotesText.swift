// The owner's notes, read for a phone screen (App/NotesView.swift, the Notes app; link_notes.py; docs/app-chat.md).
// Owner, 2026-10-10: "an app where I can look at my notes and interact with them change delete updates", "essentially
// like a mini notion", "make sure it's mobile native". The notes are the second brain's markdown vault; the Mac
// answers with JSON and markdown, and this turns them into rows, folders and a page's blocks. Foundation only, so
// Tests/main.swift checks it on the Mac.
import Foundation

/// One note in the list: where it is, its title, its first line, and when it last changed.
struct NoteSummary: Equatable, Sendable, Hashable, Identifiable {
    var path: String
    var folder: String
    var title: String
    var preview: String
    var modified: Date
    var id: String { path }
}

/// A search hit inside a note's text.
struct NoteHit: Equatable, Sendable, Hashable, Identifiable {
    var path: String
    var title: String
    var snippet: String
    var id: String { path }
}

/// The Notes panel's answer (link_notes.py): a list, a page, or what a save, delete, move or restore did.
struct NotesReply: Equatable, Sendable {
    var ok: Bool
    var line: String
    var notes: [NoteSummary] = []
    var folders: [String] = []
    var hits: [NoteHit] = []
    var path: String?
    var title: String?
    var body: String?
    var front: [String] = []
    var revision: String?
    var editable = true
    var undoID: String?
    var trashID: String?
    var changed: Bool?

    static func decode(_ data: Data) -> NotesReply? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let notes = (obj["notes"] as? [[String: Any]] ?? []).compactMap { n -> NoteSummary? in
            guard let path = n["path"] as? String, !path.isEmpty else { return nil }
            let seconds = (n["modified"] as? NSNumber)?.doubleValue ?? 0
            return NoteSummary(path: path, folder: n["folder"] as? String ?? "", title: n["title"] as? String ?? path,
                               preview: n["preview"] as? String ?? "", modified: Date(timeIntervalSince1970: seconds))
        }
        let hits = (obj["hits"] as? [[String: Any]] ?? []).compactMap { h -> NoteHit? in
            guard let path = h["path"] as? String, !path.isEmpty else { return nil }
            return NoteHit(path: path, title: h["title"] as? String ?? path, snippet: h["snippet"] as? String ?? "")
        }
        func text(_ key: String) -> String? { (obj[key] as? String).flatMap { $0.isEmpty ? nil : $0 } }
        return NotesReply(ok: obj["ok"] as? Bool ?? false, line: obj["line"] as? String ?? "", notes: notes,
                          folders: (obj["folders"] as? [String] ?? []).filter { !$0.isEmpty }, hits: hits,
                          path: text("path"), title: text("title"), body: obj["body"] as? String,
                          front: (obj["front"] as? [String] ?? []).filter { !$0.isEmpty }, revision: text("revision"),
                          editable: obj["editable"] as? Bool ?? true, undoID: text("undo_id"),
                          trashID: text("trash_id"), changed: obj["changed"] as? Bool)
    }
}

/// A folder of the list: its path, how it reads, its symbol, and its notes, newest first.
struct NoteFolder: Equatable, Sendable, Identifiable {
    var path: String
    var name: String
    var symbol: String
    var notes: [NoteSummary]
    var id: String { path }
}

/// One block of a page, with the line of the body it starts on (a to-do's line is the one a tap toggles).
struct NoteBlock: Equatable, Sendable, Identifiable {
    enum Kind: Equatable, Sendable {
        case heading(Int)            // 1 to 6
        case todo(done: Bool)
        case bullet
        case numbered(String)        // "1." as written
        case quote
        case code
        case rule
        case text
    }
    var kind: Kind
    var text: String
    var line: Int
    var indent = 0                   // nesting, in steps of two spaces or a tab
    var id: Int { line }
}

enum NotesText {
    /// The vault's top folders as they read on a phone (second_brain.PARA), and their SF Symbols.
    static let folderNames: [String: (String, String)] = [
        "00-vision": ("Vision", "sparkles"), "01-inbox": ("Inbox", "tray"), "02-todos": ("To-dos", "checklist"),
        "03-projects": ("Projects", "folder"), "04-areas": ("Areas", "square.grid.2x2"),
        "05-resources": ("Resources", "books.vertical"), "06-processes": ("Processes", "gearshape.2"),
        "07-events": ("Events", "calendar"), "08-journals": ("Journals", "book"), "09-archive": ("Archive", "archivebox"),
    ]

    /// "03-projects/garden" → "Projects · Garden"; "01-inbox" → "Inbox"; "" → "Vault".
    static func folderName(_ path: String) -> String {
        if path.isEmpty { return "Vault" }
        var parts = path.split(separator: "/").map(String.init)
        let top = parts.removeFirst()
        let head = folderNames[top]?.0 ?? words(top)
        return ([head] + parts.map(words)).joined(separator: " · ")
    }

    static func folderSymbol(_ path: String) -> String {
        let top = path.split(separator: "/").first.map(String.init) ?? ""
        return path.isEmpty ? "doc.text" : folderNames[top]?.1 ?? "folder"
    }

    /// "garden-beds" → "Garden beds"; a leading "NN-" goes.
    static func words(_ name: String) -> String {
        var s = name
        if let r = s.range(of: #"^\d{2}-"#, options: .regularExpression) { s.removeSubrange(r) }
        s = s.replacingOccurrences(of: "-", with: " ").replacingOccurrences(of: "_", with: " ")
        return s.prefix(1).uppercased() + s.dropFirst()
    }

    /// The list's folders: in the vault's order (the folders the Mac gave, then any other), each newest first. The
    /// root's own notes (the vault guide) come last, and empty folders are left out.
    static func group(_ notes: [NoteSummary], order: [String]) -> [NoteFolder] {
        let byFolder = Dictionary(grouping: notes, by: \.folder)
        func rank(_ folder: String) -> (Int, String) {
            if folder.isEmpty { return (Int.max, "") }
            if let i = order.firstIndex(of: folder) { return (i * 2, folder) }
            let top = String(folder.split(separator: "/").first ?? "")
            if let i = order.firstIndex(of: top) { return (i * 2 + 1, folder) }   // after its parent
            return (order.count * 2, folder)
        }
        return byFolder.keys.sorted { rank($0) < rank($1) }.map { folder in
            NoteFolder(path: folder, name: folderName(folder), symbol: folderSymbol(folder),
                       notes: (byFolder[folder] ?? []).sorted { ($0.modified, $1.path) > ($1.modified, $0.path) })
        }
    }

    /// Notes whose title, first line or folder has every typed word, for the search field as you type.
    static func filter(_ notes: [NoteSummary], _ query: String) -> [NoteSummary] {
        let words = query.lowercased().split(whereSeparator: \.isWhitespace).map(String.init)
        if words.isEmpty { return notes }
        return notes.filter { n in
            let hay = (n.title + " " + n.preview + " " + folderName(n.folder)).lowercased()
            return words.allSatisfy { hay.contains($0) }
        }
    }

    /// A row's date, as Notes has it: the time today, "Yesterday", the weekday this week, else the day and month
    /// (and the year when it is not this one).
    static func stamp(_ date: Date, now: Date = Date(), calendar: Calendar = .current) -> String {
        if calendar.isDate(date, inSameDayAs: now) { return date.formatted(date: .omitted, time: .shortened) }
        if let y = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(date, inSameDayAs: y) {
            return "Yesterday"
        }
        if let week = calendar.date(byAdding: .day, value: -6, to: now), date > calendar.startOfDay(for: week), date < now {
            return date.formatted(.dateTime.weekday(.wide))
        }
        if calendar.component(.year, from: date) == calendar.component(.year, from: now) {
            return date.formatted(.dateTime.day().month(.abbreviated))
        }
        return date.formatted(.dateTime.day().month(.abbreviated).year())
    }

    // MARK: A page

    private static let todo = try! NSRegularExpression(pattern: #"^([-*+])\s+\[( |x|X)\]\s?(.*)$"#)
    private static let bullet = try! NSRegularExpression(pattern: #"^[-*+]\s+(.*)$"#)
    private static let numbered = try! NSRegularExpression(pattern: #"^(\d{1,3}[.)])\s+(.*)$"#)
    private static let heading = try! NSRegularExpression(pattern: #"^(#{1,6})\s+(.*?)(?:\s+#+)?\s*$"#)

    /// A page's body as blocks, one per line of markdown, a fenced code block as one. Blank lines are left out.
    static func blocks(_ body: String) -> [NoteBlock] {
        let lines = body.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")
        var out: [NoteBlock] = []
        var i = 0
        while i < lines.count {
            let raw = lines[i]
            let lead = raw.prefix { $0 == " " || $0 == "\t" }
            let indent = lead.reduce(0) { $0 + ($1 == "\t" ? 2 : 1) } / 2
            let line = raw.trimmingCharacters(in: .whitespaces)
            defer { i += 1 }
            if line.isEmpty { continue }
            if line.hasPrefix("```") {
                var code: [String] = []
                let start = i
                i += 1
                while i < lines.count, !lines[i].trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                    code.append(lines[i])
                    i += 1
                }
                out.append(NoteBlock(kind: .code, text: code.joined(separator: "\n"), line: start))
                continue
            }
            if line.count >= 3, Set(line.replacingOccurrences(of: " ", with: "")).count == 1,
               ["-", "*", "_"].contains(line.first.map(String.init) ?? "") {
                out.append(NoteBlock(kind: .rule, text: "", line: i))
                continue
            }
            if let m = match(heading, line) {
                out.append(NoteBlock(kind: .heading(m[1].count), text: m[2], line: i))
            } else if let m = match(todo, line) {
                out.append(NoteBlock(kind: .todo(done: m[2] != " "), text: m[3], line: i, indent: indent))
            } else if let m = match(bullet, line) {
                out.append(NoteBlock(kind: .bullet, text: m[1], line: i, indent: indent))
            } else if let m = match(numbered, line) {
                out.append(NoteBlock(kind: .numbered(m[1]), text: m[2], line: i, indent: indent))
            } else if line.hasPrefix(">") {
                out.append(NoteBlock(kind: .quote, text: String(line.dropFirst()).trimmingCharacters(in: .whitespaces),
                                     line: i))
            } else {
                out.append(NoteBlock(kind: .text, text: line, line: i, indent: indent))
            }
        }
        return out
    }

    /// The body with the to-do on that line checked or unchecked; nil when that line is not a to-do.
    static func toggle(_ body: String, line: Int) -> String? {
        var lines = body.components(separatedBy: "\n")
        guard lines.indices.contains(line) else { return nil }
        let raw = lines[line]
        let lead = String(raw.prefix { $0 == " " || $0 == "\t" })
        let rest = String(raw.dropFirst(lead.count))
        guard let m = match(todo, rest.trimmingCharacters(in: CharacterSet(charactersIn: "\r"))) else { return nil }
        let mark = m[2] == " " ? "x" : " "
        let cr = rest.hasSuffix("\r") ? "\r" : ""
        lines[line] = lead + m[1] + " [" + mark + "] " + m[3] + cr
        return lines.joined(separator: "\n")
    }

    /// Markdown's inline marks for the screen: [[wiki links]] read as their words (Obsidian's alias after "|").
    static func inline(_ text: String) -> String {
        guard text.contains("[[") else { return text }
        let r = try! NSRegularExpression(pattern: #"\[\[([^\]|]+)(?:\|([^\]]+))?\]\]"#)
        let ns = text as NSString
        var out = ""
        var last = 0
        for m in r.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
            out += ns.substring(with: NSRange(location: last, length: m.range.location - last))
            let alias = m.range(at: 2)
            out += ns.substring(with: alias.location != NSNotFound ? alias : m.range(at: 1))
            last = m.range.location + m.range.length
        }
        return out + ns.substring(from: last)
    }

    private static func match(_ r: NSRegularExpression, _ s: String) -> [String]? {
        let ns = s as NSString
        guard let m = r.firstMatch(in: s, range: NSRange(location: 0, length: ns.length)) else { return nil }
        return (0..<m.numberOfRanges).map { m.range(at: $0).location == NSNotFound ? "" : ns.substring(with: m.range(at: $0)) }
    }
}
