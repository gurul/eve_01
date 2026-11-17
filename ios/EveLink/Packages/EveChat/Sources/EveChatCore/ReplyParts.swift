// How one of eve's replies splits into what to draw (docs/app-chat.md). eve answers in plain markdown most of the
// time. When a table, chart, list, steps or choices help, it writes a card: a ```json-render fence holding one JSON
// patch per line, drawn natively (EveCards). Text outside a fence is markdown.
//
// A reply from before the app went native may hold an ```openui-lang fence (the web page's other card engine). The
// app has no renderer for it, so it is kept as its own part and drawn as one short note, never as a block of code.
import Foundation

public enum ReplyPart: Equatable, Sendable {
    case markdown(String)
    /// A json-render block: its lines so far, and whether its fence has closed.
    case card(code: String, complete: Bool)
    /// A card in the web page's older format, which this app does not draw.
    case legacyCard

    public var isCard: Bool {
        if case .card = self { return true }
        return false
    }
}

public enum ReplyParts {
    static let cardFence = "```json-render"
    static let legacyFence = "```openui-lang"
    static let fences = [cardFence, legacyFence]

    private enum FenceKind { case card, legacy }

    /// The first card fence in `text`: where it opens and which kind it is.
    private static func nextFence(_ text: Substring) -> (at: Substring.Index, kind: FenceKind)? {
        let card = text.range(of: cardFence)?.lowerBound
        let legacy = text.range(of: legacyFence)?.lowerBound
        switch (card, legacy) {
        case (nil, nil): return nil
        case let (c?, nil): return (c, .card)
        case let (nil, l?): return (l, .legacy)
        case let (c?, l?): return c < l ? (c, .card) : (l, .legacy)
        }
    }

    /// `root = Card(` at the start of a line: a bare program in the older format, not prose that says "root = 5".
    private static let legacyRoot = try! NSRegularExpression(pattern: #"(^|\n)[ \t]*root[ \t]*=[ \t]*[A-Z][A-Za-z0-9]*\("#)

    /// Drop a half-typed fence opener at the very end of a streaming reply.
    private static func trimPartialFence(_ text: Substring) -> Substring {
        let lineStart = text.lastIndex(of: "\n").map { text.index(after: $0) } ?? text.startIndex
        let tail = text[lineStart...]
        if tail.hasPrefix("`"), fences.contains(where: { $0.hasPrefix(tail) }) { return text[..<lineStart] }
        return text
    }

    private static func pushMarkdown(_ parts: inout [ReplyPart], _ text: Substring) {
        let trimmed = text.trimmingCharacters(in: .newlines)
        if !trimmed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { parts.append(.markdown(trimmed)) }
    }

    public static func split(_ raw: String, streaming: Bool) -> [ReplyPart] {
        var parts: [ReplyPart] = []
        var rest = Substring(raw)

        if nextFence(rest) == nil {
            let ns = raw as NSString
            if let m = legacyRoot.firstMatch(in: raw, range: NSRange(location: 0, length: ns.length)) {
                // A bare program: the prose before `root =` stays prose (unless that is inside some other fence).
                let before = ns.substring(to: m.range.location + (m.range.length > 0 ? 1 : 0))
                if !before.contains("```") {
                    pushMarkdown(&parts, Substring(ns.substring(to: m.range.location)))
                    parts.append(.legacyCard)
                    return parts
                }
            }
            pushMarkdown(&parts, streaming ? trimPartialFence(rest) : rest)
            return parts
        }

        while !rest.isEmpty {
            guard let fence = nextFence(rest) else {
                pushMarkdown(&parts, streaming ? trimPartialFence(rest) : rest)
                break
            }
            pushMarkdown(&parts, rest[..<fence.at])
            let part: (String, Bool) -> ReplyPart = { code, complete in
                fence.kind == .card ? .card(code: code, complete: complete) : .legacyCard
            }
            guard let openEnd = rest[fence.at...].firstIndex(of: "\n") else {
                parts.append(part("", false))                       // only the opener has arrived
                break
            }
            let bodyStart = rest.index(after: openEnd)
            // The closing fence is "\n```", searched from the opener's own newline: an empty block closes at once.
            guard let close = rest[openEnd...].range(of: "\n```") else {
                parts.append(part(String(rest[bodyStart...]), false))
                break
            }
            parts.append(part(close.lowerBound >= bodyStart ? String(rest[bodyStart..<close.lowerBound]) : "", true))
            if let after = rest[close.upperBound...].firstIndex(of: "\n") {
                rest = rest[rest.index(after: after)...]
            } else {
                rest = ""
            }
        }
        return parts
    }
}

/// The owner's side of a card tap (docs/app-chat.md, "Using generated cards"). A tap that continues the conversation
/// goes to the daemon as its words, a blank line, and a JSON list of what was clicked and any form values, so the
/// brain sees both. The bubble shows the words alone, live and when the line is read back from history.
public enum OwnerAction {
    /// What the daemon is sent: `human`, then the context as one line of JSON. `extra` holds JSON values (strings,
    /// numbers, booleans, lists and objects of them).
    public static func text(human: String, extra: [Any] = []) -> String {
        let context: [Any] = ["User clicked: \(human)"] + extra
        guard JSONSerialization.isValidJSONObject(context),
              let data = try? JSONSerialization.data(withJSONObject: context, options: [.withoutEscapingSlashes, .sortedKeys])
        else { return human.trimmingCharacters(in: .whitespacesAndNewlines) }
        return "\(human)\n\n\(String(decoding: data, as: UTF8.self))".trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static let contextTail = try! NSRegularExpression(pattern: #"\n\n(\["User clicked: [\s\S]*\])$"#)

    /// An owner line as the bubble shows it: a card tap's words, without the context JSON that went with them.
    public static func display(_ text: String) -> String {
        let ns = text as NSString
        guard let m = contextTail.firstMatch(in: text, range: NSRange(location: 0, length: ns.length)),
              let data = ns.substring(with: m.range(at: 1)).data(using: .utf8),
              (try? JSONSerialization.jsonObject(with: data)) is [Any] else { return text }
        return ns.substring(to: m.range.location)
    }
}
