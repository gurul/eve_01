// A reply that is still being written ends mid-construct: an unclosed "**", half a link, a table with only its
// header. This rewrites the end of the source so the half-made part draws quietly: a mark with nothing after it
// waits, an open bold or code span is closed where the text stops, a link without its address shows its words.
// Only the last block is touched, and only while the reply streams.
import Foundation

enum StreamingRepair {
    static func repair(_ source: String) -> String {
        var lines = source.split(separator: "\n", omittingEmptySubsequences: false)
        // The text after the last line break: empty when the source ends on a break.
        let tail = lines.removeLast()
        var fence = FenceTracker()
        for line in lines { fence.read(line) }

        var open: Substring? = tail.isEmpty ? nil : tail
        if let last = open {
            if fence.isOpen {
                // Inside code only the first marks of the closing fence wait.
                return fence.couldClose(last) ? joined(lines, open: nil) : source
            }
            if isOnlyMarks(last) {
                open = nil
            } else if fence.read(last) {
                return source
            }
        } else if fence.isOpen {
            return source
        }

        // A table's first row waits for the row of dashes under it.
        let all = lines + (open.map { [$0] } ?? [])
        let group = all.reversed().prefix { !$0.isBlankLine }
        if group.count == 1, let only = group.first, only.withoutQuoteMarks.hasPrefix("|") {
            if open != nil { open = nil } else { lines.removeLast() }
            return joined(lines, open: open)
        }

        // The inline marks of the paragraph that is being written.
        var body = all
        guard let last = body.last, !last.isBlankLine else { return joined(lines, open: open) }
        var first = body.count - 1
        if last.withoutQuoteMarks.hasPrefix("|") {
            // A table row: only the cell being written.
            let cells = lastCell(of: last)
            body[first] = Substring(String(last[..<cells]) + String(repairInline(Array(last[cells...]))))
        } else {
            while first > 0, !startsBlock(body[first]), !endsBlock(body[first - 1]) { first -= 1 }
            let lead = inlineStart(of: body[first])
            let region = String(body[first][lead...]) + body[(first + 1)...].map { "\n" + $0 }.joined()
            let fixed = String(body[first][..<lead]) + String(repairInline(Array(region)))
            body.replaceSubrange(first..., with: [Substring(fixed)])
        }
        return body.joined(separator: "\n") + (open == nil ? "\n" : "")
    }

    private static func joined(_ lines: [Substring], open: Substring?) -> String {
        lines.map { $0 + "\n" }.joined() + (open ?? "")
    }

    /// True for a last line that holds nothing but the marks that start a block: "-", "1.", "##", "```", "|--",
    /// "- [", "**". What it becomes depends on the next characters, so it is not drawn yet.
    static func isOnlyMarks(_ line: Substring) -> Bool {
        var rest = line.withoutQuoteMarks
        switch ListMarker(rest) {
        case .partial: return true
        case .full(let n):
            rest = rest.dropFirst(n).drop { $0 == " " || $0 == "\t" }
            if !rest.isEmpty, ["[ ] ", "[x] ", "[X] "].contains(where: { $0.hasPrefix(rest) }) { return true }
        case .none: break
        }
        return rest.allSatisfy { "-=*_+#~`|: \t".contains($0) }
    }

    /// True when the line starts a block of its own, so the paragraph being written cannot reach above it.
    private static func startsBlock(_ line: Substring) -> Bool {
        let rest = line.withoutQuoteMarks
        if rest.startIndex != line.startIndex { return true }
        guard let first = rest.first else { return true }
        return ListMarker(rest) != .none || first == "#" || first == "|" || rest.hasPrefix("```") || rest.hasPrefix("~~~")
    }

    /// True when the line cannot have a paragraph run on from it: a blank, a heading, a table row, a fence.
    private static func endsBlock(_ line: Substring) -> Bool {
        let rest = line.withoutQuoteMarks
        guard let first = rest.first else { return true }
        return first == "#" || first == "|" || rest.hasPrefix("```") || rest.hasPrefix("~~~")
    }

    /// Where the inline text of a block's first line begins: past quote marks, a list marker, a task box, "#"s.
    private static func inlineStart(of line: Substring) -> Substring.Index {
        var rest = line.withoutQuoteMarks
        if case .full(let n) = ListMarker(rest) {
            rest = rest.dropFirst(n).drop { $0 == " " || $0 == "\t" }
            if rest.hasPrefix("[ ] ") || rest.hasPrefix("[x] ") || rest.hasPrefix("[X] ") { rest = rest.dropFirst(4) }
        }
        rest = rest.drop { $0 == "#" }.drop { $0 == " " }
        return rest.startIndex
    }

    /// The start of a table row's last cell: after the last "|" that is not escaped.
    private static func lastCell(of line: Substring) -> Substring.Index {
        var index = line.endIndex
        while index > line.startIndex {
            let before = line.index(before: index)
            if line[before] == "|", before == line.startIndex || line[line.index(before: before)] != "\\" { return index }
            index = before
        }
        return line.startIndex
    }

    // MARK: Inline marks

    private struct Opener {
        var mark: Character
        var count: Int
        /// Where its run ends in the output: an opener with nothing after it is taken out again.
        var end: Int
    }

    private enum Bracket {
        /// "[words](address)": the address is copied through `end` without a second look.
        case link(close: Int, end: Int)
        /// "[words" with no "]" yet: the bracket waits.
        case unclosed
        /// "[words]" at the very end: both brackets wait for the "(".
        case closedAtEnd(Int)
        /// "[words](addr": the words show, the half address waits.
        case halfAddress(close: Int)
        case literal
    }

    /// How far ahead a "[", a "<" or a run of backticks looks for its other end. A paragraph of nothing but
    /// openers would otherwise cost its length squared.
    private static let reach = 2_000

    /// Closes the marks that are open where the text stops. Marks that have matched are left as written.
    static func repairInline(_ chars: [Character]) -> [Character] {
        let count = chars.count
        // A paragraph this long is not prose; it is drawn as written.
        guard count <= 20_000 else { return chars }
        var out: [Character] = []
        out.reserveCapacity(count + 8)
        var openers: [Opener] = []
        var skipped: Set<Int> = []
        /// The "]" of a whole link, and where its address ends.
        var linkEnds: [Int: Int] = [:]
        var stop = count
        var i = 0
        scan: while i < stop {
            let c = chars[i]
            if skipped.contains(i) { i += 1; continue }
            switch c {
            case "\\":
                out.append(c)
                if i + 1 < stop { out.append(chars[i + 1]) }
                i += 2
            case "`":
                var run = 1
                while i + run < count, chars[i + run] == "`" { run += 1 }
                if let close = closingBackticks(chars, from: i + run, run: run) {
                    out.append(contentsOf: chars[i..<(close + run)])
                    i = close + run
                } else {
                    // An open code span: the rest is code so far. One with no code yet waits.
                    if i + run < stop {
                        out.append(contentsOf: chars[i..<stop])
                        while let last = out.last, last == " " || last == "\n" { out.removeLast() }
                        out.append(contentsOf: repeatElement("`", count: run))
                    }
                    break scan
                }
            case "<":
                // "<https://exam" with no ">" yet: the bare address is a link by itself.
                let rest = chars[(i + 1)..<min(count, i + 1 + reach)]
                if rest.starts(with: ["h", "t", "t", "p"]), !rest.contains(">") { i += 1 } else { out.append(c); i += 1 }
            case "!" where i + 1 < count && chars[i + 1] == "[":
                // A picture shows once its address is whole.
                if case .link(_, let end) = bracket(chars, at: i + 1) {
                    out.append(contentsOf: chars[i...end])
                    i = end + 1
                } else {
                    break scan
                }
            case "[":
                switch bracket(chars, at: i) {
                case .link(let close, let end):
                    out.append(c)
                    linkEnds[close] = end
                case .unclosed: break
                case .closedAtEnd(let close): skipped.insert(close)
                case .halfAddress(let close): stop = min(stop, close)
                case .literal: out.append(c)
                }
                i += 1
            case "]":
                out.append(c)
                if let end = linkEnds[i] {
                    out.append(contentsOf: chars[(i + 1)...end])
                    i = end + 1
                } else {
                    i += 1
                }
            case "*", "_", "~":
                var run = 1
                while i + run < count, chars[i + run] == c { run += 1 }
                let before: Character = i > 0 ? chars[i - 1] : " "
                let after: Character? = i + run < stop ? chars[i + run] : nil
                if c == "~", run != 2 {
                    // One "~" is text, unless it is the first half of the "~~" that ends a strikethrough.
                    let halfCloser = run == 1 && after == nil && openers.contains { $0.mark == "~" }
                    if !halfCloser { out.append(contentsOf: repeatElement(c, count: run)) }
                    i += run
                    continue
                }
                let word: (Character) -> Bool = { $0.isLetter || $0.isNumber }
                let canOpen = after.map { !$0.isWhitespace } == true && !(c == "_" && word(before))
                let canClose = !before.isWhitespace && !(c == "_" && after.map(word) == true)
                var left = run
                if canClose {
                    while left > 0, let top = openers.lastIndex(where: { $0.mark == c }) {
                        if openers[top].count <= left {
                            left -= openers[top].count
                            openers.removeSubrange(top...)
                        } else {
                            openers[top].count -= left
                            openers.removeSubrange((top + 1)...)
                            left = 0
                        }
                    }
                }
                if left > 0, after == nil {
                    // Marks at the very end: an opener that has just arrived, or half of a closer. They wait.
                    out.append(contentsOf: repeatElement(c, count: run - left))
                } else {
                    out.append(contentsOf: repeatElement(c, count: run))
                    if left > 0, canOpen { openers.append(Opener(mark: c, count: left, end: out.count)) }
                }
                i += run
            default:
                out.append(c)
                i += 1
            }
        }
        // Close what is still open, innermost first, in front of any trailing space.
        var space: [Character] = []
        while let last = out.last, last == " " || last == "\t" || last == "\n" {
            space.append(last)
            out.removeLast()
        }
        for opener in openers.reversed() {
            if opener.end >= out.count {
                out.removeLast(min(opener.count, out.count))
            } else {
                out.append(contentsOf: repeatElement(opener.mark, count: opener.count))
            }
        }
        return out + space.reversed()
    }

    private static func closingBackticks(_ chars: [Character], from start: Int, run: Int) -> Int? {
        var i = start
        let limit = min(chars.count, start + reach)
        while i < limit {
            guard chars[i] == "`" else { i += 1; continue }
            var length = 1
            while i + length < chars.count, chars[i + length] == "`" { length += 1 }
            if length == run { return i }
            i += length
        }
        return nil
    }

    private static func bracket(_ chars: [Character], at open: Int) -> Bracket {
        let count = chars.count
        let limit = min(count, open + reach)
        var depth = 1
        var i = open + 1
        while i < limit {
            switch chars[i] {
            case "\\": i += 1
            case "[": depth += 1
            case "]": depth -= 1
            default: break
            }
            if depth == 0 { break }
            i += 1
        }
        guard i < limit else { return count > limit ? .literal : .unclosed }
        let close = i
        guard close + 1 < count else { return .closedAtEnd(close) }
        guard chars[close + 1] == "(" else { return .literal }
        depth = 1
        i = close + 2
        while i < limit {
            switch chars[i] {
            case "\\": i += 1
            case "(": depth += 1
            case ")": depth -= 1
            default: break
            }
            if depth == 0 { return .link(close: close, end: i) }
            i += 1
        }
        return count > limit ? .literal : .halfAddress(close: close)
    }
}
