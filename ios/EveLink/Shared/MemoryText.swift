// What eve remembers, read for a phone screen (App/ControlSheets.swift, the Memory card; docs/stackchan/memory.md).
//
// The daemon hands the profile over as the records' own markdown (a `---` front matter, a `#` title, `##`
// sections, `- ` facts that end in "(said 2026-09-22)." and name records as [[project-storeybox]]), and the search's
// facts and recalled meanings carry the same marks. Shown raw they read like a file (owner, 2026-10-09: "make the
// memories page look better"). Foundation only, so Tests/main.swift checks it on the Mac.
import Foundation

/// One fact, with the day it was said when the line carried one.
struct MemoryItem: Equatable, Sendable, Hashable {
    var text: String
    var said: String?           // "2026-09-22"
}

/// One `##` section of the profile.
struct MemorySection: Equatable, Sendable, Hashable, Identifiable {
    var title: String
    var items: [MemoryItem]
    var id: String { title }
}

/// The profile as the sheet draws it.
struct MemoryProfile: Equatable, Sendable {
    var updated: String?        // "2026-10-08", from the front matter
    var intro: String?          // the paragraph under the title, if any
    var sections: [MemorySection]

    var isEmpty: Bool { sections.isEmpty && (intro ?? "").isEmpty }
}

enum MemoryText {
    private static let wikilink = try! NSRegularExpression(pattern: #"\[\[([^\]]+)\]\]"#)
    // "(said 2026-09-22).", "(owner, 2026-09-29)", "(2026-09-22)": the day a fact was said, however the records put it
    private static let saidTail = try! NSRegularExpression(pattern: #"\s*\((?:[a-z]+[\s,]+)?(\d{4}-\d{2}-\d{2})\)\.?\s*$"#)
    private static let recalledHead = try! NSRegularExpression(pattern: #"^\(([^)]{1,80})\)\s*"#)
    private static let spaces = try! NSRegularExpression(pattern: #"\s{2,}"#)
    // a record's name alone in parentheses at the end ("(seattle-scavenger-hunt)"): a pointer, not words
    private static let slugTail = try! NSRegularExpression(pattern: #"\s*\((?:[a-z0-9]+(?:-[a-z0-9]+)+)\)\s*$"#)
    private static let linkTail = try! NSRegularExpression(pattern: #"(?:\s*\(\[\[[^\]]+\]\]\)\.?)+\s*$"#)

    /// The profile's markdown as sections of facts. The front matter and the `#` title go; a paragraph before the
    /// first section is the intro; lines that are neither a heading nor a bullet join the item before them.
    static func profile(_ raw: String) -> MemoryProfile {
        var lines = raw.components(separatedBy: "\n")
        var updated: String?
        if lines.first?.trimmingCharacters(in: .whitespaces) == "---",
           let end = lines.dropFirst().firstIndex(where: { $0.trimmingCharacters(in: .whitespaces) == "---" }) {
            for l in lines[1..<end] where l.hasPrefix("updated:") {
                updated = l.dropFirst("updated:".count).trimmingCharacters(in: .whitespaces)
            }
            lines.removeSubrange(0...end)
        }
        var intro: [String] = []
        var sections: [MemorySection] = []
        for rawLine in lines {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if line.isEmpty || line.hasPrefix("# ") { continue }
            if line.hasPrefix("## ") {
                sections.append(MemorySection(title: clean(String(line.dropFirst(3))), items: []))
            } else if line.hasPrefix("- ") || line.hasPrefix("* ") {
                let item = item(String(line.dropFirst(2)))
                if sections.isEmpty { intro.append(item.text) } else { sections[sections.count - 1].items.append(item) }
            } else if sections.isEmpty {
                intro.append(clean(line))
            } else if var last = sections[sections.count - 1].items.last {
                // a fact wrapped onto a second line
                let more = item(line)
                last.text += " " + more.text
                if last.said == nil { last.said = more.said }
                sections[sections.count - 1].items[sections[sections.count - 1].items.count - 1] = last
            }
        }
        let introText = intro.joined(separator: " ").trimmingCharacters(in: .whitespaces)
        return MemoryProfile(updated: updated, intro: introText.isEmpty ? nil : introText,
                             sections: sections.filter { !$0.items.isEmpty })
    }

    /// A fact line as an item: the "(said …)" tail becomes the day, the rest is cleaned.
    static func item(_ line: String) -> MemoryItem {
        var text = line
        var said: String?
        let whole = NSRange(text.startIndex..., in: text)
        if let m = saidTail.firstMatch(in: text, range: whole), let r = Range(m.range(at: 1), in: text),
           let all = Range(m.range, in: text) {
            said = String(text[r])
            text.removeSubrange(all)
        }
        return MemoryItem(text: clean(text), said: said)
    }

    /// A recalled meaning "(2026-10-01, app) likes tea" as its caption and text.
    static func recalled(_ line: String) -> (caption: String, text: String) {
        let whole = NSRange(line.startIndex..., in: line)
        guard let m = recalledHead.firstMatch(in: line, range: whole), let r = Range(m.range(at: 1), in: line),
              let all = Range(m.range, in: line) else { return ("", clean(line)) }
        let parts = line[r].split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
        let caption = parts.map { p -> String in
            let day = p.prefix(10)
            return day.count == 10 && day.filter({ $0 == "-" }).count == 2 ? dayLabel(String(day)) + p.dropFirst(10) : p
        }.joined(separator: " · ")
        var rest = line
        rest.removeSubrange(all)
        return (caption, clean(rest))
    }

    /// Wikilinks as words ([[project-storeybox]] → storeybox), ending punctuation, runs of spaces.
    static func clean(_ s: String) -> String {
        var text = s.trimmingCharacters(in: .whitespacesAndNewlines)
        while text.hasSuffix(".") || text.hasSuffix(" ") { text.removeLast() }
        // a record named alone in parentheses at the end, "([[project-storeybox]])": a pointer, not words
        text = linkTail.stringByReplacingMatches(in: text, range: NSRange(text.startIndex..., in: text), withTemplate: "")
        let whole = NSRange(text.startIndex..., in: text)
        text = wikilink.stringByReplacingMatches(in: text, range: whole, withTemplate: "$1")
        text = text.replacingOccurrences(of: "project-", with: "")
            .replacingOccurrences(of: "([", with: "(").replacingOccurrences(of: "])", with: ")")
        while text.hasSuffix(".") || text.hasSuffix(" ") { text.removeLast() }
        text = slugTail.stringByReplacingMatches(in: text, range: NSRange(text.startIndex..., in: text), withTemplate: "")
        text = spaces.stringByReplacingMatches(in: text, range: NSRange(text.startIndex..., in: text), withTemplate: " ")
        text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        while text.hasSuffix(".") || text.hasSuffix(" ") { text.removeLast() }
        text = text.replacingOccurrences(of: " ()", with: "").replacingOccurrences(of: "()", with: "")
        return text
    }

    /// "2026-09-22" → "Sep 22" (this year) or "Sep 22, 2025"; anything else as it is.
    static func dayLabel(_ iso: String, today: Date = .now) -> String {
        let parts = iso.split(separator: "-").compactMap { Int($0) }
        guard parts.count == 3 else { return iso }
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = .current
        guard let date = cal.date(from: DateComponents(year: parts[0], month: parts[1], day: parts[2])) else { return iso }
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.setLocalizedDateFormatFromTemplate(cal.component(.year, from: date) == cal.component(.year, from: today) ? "MMM d" : "MMM d yyyy")
        return f.string(from: date)
    }

    /// Who said a transcript line, for a caption: the owner is "you".
    static func whoLabel(_ who: String) -> String {
        switch who.lowercased() {
        case "owner", "user", "you": "you"
        case "eve", "assistant": "eve"
        case "": ""
        default: who
        }
    }
}
