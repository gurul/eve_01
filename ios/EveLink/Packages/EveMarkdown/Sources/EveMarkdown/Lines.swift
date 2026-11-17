// Line-level reading of markdown source, shared by the splitter and the streaming repair: is a line blank, does it
// start a list item, is a code fence open. It is a light scan, not a parser; Foundation does the parsing.
import Foundation

extension Substring {
    var isBlankLine: Bool { allSatisfy { $0 == " " || $0 == "\t" } }

    /// The line without its indent and its block quote marks.
    var withoutQuoteMarks: Substring {
        var rest = self
        while let first = rest.first, first == " " || first == "\t" || first == ">" { rest = rest.dropFirst() }
        return rest
    }
}

/// What a line starts with, read as a list marker.
enum ListMarker: Equatable {
    case none
    /// "-" or "12" or "12." at the end of the text: a marker if a space comes next.
    case partial(Int)
    /// "- " or "12. ": the count covers the marker and its one space.
    case full(Int)

    init(_ line: Substring) {
        guard let first = line.first else { self = .none; return }
        var length = 0
        if first == "-" || first == "+" || first == "*" {
            length = 1
        } else if first.isASCII, first.isNumber {
            let digits = line.prefix { $0.isASCII && $0.isNumber }.count
            guard digits <= 9 else { self = .none; return }
            let after = line.dropFirst(digits)
            guard let stop = after.first else { self = .partial(digits); return }
            guard stop == "." || stop == ")" else { self = .none; return }
            length = digits + 1
        } else {
            self = .none
            return
        }
        guard let next = line.dropFirst(length).first else { self = .partial(length); return }
        self = next == " " || next == "\t" ? .full(length + 1) : .none
    }
}

/// Follows code fences down the lines, so the splitter and the repair leave code alone.
struct FenceTracker {
    private var mark: Character?
    private var count = 0

    var isOpen: Bool { mark != nil }

    /// The line's content where a fence would be: past the indent, quote marks and one list marker.
    private static func content(_ line: Substring) -> Substring {
        var rest = line.withoutQuoteMarks
        if case .full(let n) = ListMarker(rest) { rest = rest.dropFirst(n).withoutQuoteMarks }
        return rest
    }

    /// Reads one line. True when the line is a fence or is code inside one.
    @discardableResult
    mutating func read(_ line: Substring) -> Bool {
        let rest = Self.content(line)
        if let open = mark {
            let run = rest.prefix { $0 == open }.count
            if run >= count, rest.dropFirst(run).isBlankLine { mark = nil }
            return true
        }
        guard let first = rest.first, first == "`" || first == "~" else { return false }
        let run = rest.prefix { $0 == first }.count
        guard run >= 3 else { return false }
        // "```code``` and words" is a code span, not a fence.
        if first == "`", rest.dropFirst(run).contains("`") { return false }
        mark = first
        count = run
        return true
    }

    /// True when the text so far could be the start of the line that closes the open fence.
    func couldClose(_ line: Substring) -> Bool {
        guard let open = mark else { return false }
        return Self.content(line).allSatisfy { $0 == open || $0 == " " || $0 == "\t" }
    }
}

enum SourceLines {
    /// Splits a document where one top-level block ends and the next begins, so each piece parses on its own and
    /// a growing reply re-parses only its last piece. A cut is a line at the left edge, after a blank line and
    /// outside a code fence, that cannot continue a list. Text only ever adds cuts at its end.
    static func pieces(_ text: String) -> [Substring] {
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        var cuts: [String.Index] = []
        var fence = FenceTracker()
        var afterBlank = false
        for line in lines {
            if line.isBlankLine {
                afterBlank = !fence.isOpen
                continue
            }
            if afterBlank, !fence.isOpen, line.startIndex != text.startIndex,
               let first = line.first, first != " ", first != "\t", ListMarker(line) == .none {
                cuts.append(line.startIndex)
            }
            fence.read(line)
            afterBlank = false
        }
        var out: [Substring] = []
        var start = text.startIndex
        for cut in cuts {
            out.append(text[start..<cut])
            start = cut
        }
        out.append(text[start...])
        return out
    }

    /// A line of three or more dashes right under a line of text reads as a break, not as a mark that turns the
    /// text above into a heading: a blank line goes in before it.
    static func rulesNotHeadings(_ source: String) -> String {
        guard source.contains("---") else { return source }
        let lines = source.split(separator: "\n", omittingEmptySubsequences: false)
        var out: [Substring] = []
        out.reserveCapacity(lines.count + 2)
        var fence = FenceTracker()
        var changed = false
        var previousIsText = false
        for line in lines {
            let wasOpen = fence.isOpen
            let inCode = fence.read(line) || wasOpen
            if !inCode, previousIsText, isDashes(line) {
                out.append("")
                changed = true
            }
            out.append(line)
            previousIsText = !inCode && !line.isBlankLine && !isDashes(line)
        }
        return changed ? out.joined(separator: "\n") : source
    }

    private static func isDashes(_ line: Substring) -> Bool {
        let rest = line.drop { $0 == " " || $0 == "\t" }
        let dashes = rest.prefix { $0 == "-" }.count
        return dashes >= 3 && rest.dropFirst(dashes).isBlankLine
    }
}
