// Markdown source to blocks. Foundation's own parser (cmark-gfm behind AttributedString) reads each top-level
// piece of the source; this file turns its runs into the block tree and keeps the pieces that did not change, so
// a reply that grows by a word re-parses only its last block. Foundation only.
import Foundation

public enum MarkdownDocument {
    /// The blocks of a whole document.
    public static func parse(_ text: String) -> [MarkdownBlock] {
        var parser = MarkdownParser()
        return parser.parse(text)
    }
}

/// Parses a text that grows. Each call reuses the blocks of every piece of the source that is unchanged since the
/// last call, so finished blocks keep their value and their id.
public struct MarkdownParser: Sendable {
    private struct Piece: Sendable {
        var source: String
        var repaired: Bool
        var firstIndex: Int
        var blocks: [MarkdownBlock]
    }

    private var pieces: [Piece] = []
    private var lastText: String?
    private var lastStreaming = false
    private var lastBlocks: [MarkdownBlock] = []

    public init() {}

    /// `streaming` is true while the reply is still being written: the unfinished end is then drawn quietly
    /// (StreamingRepair) instead of as raw marks.
    public mutating func parse(_ text: String, streaming: Bool = false) -> [MarkdownBlock] {
        if text == lastText, streaming == lastStreaming { return lastBlocks }
        let normal = text.contains("\r") ? text.replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n") : text
        let sources = SourceLines.pieces(normal)
        var next: [Piece] = []
        next.reserveCapacity(sources.count)
        var blocks: [MarkdownBlock] = []
        for (i, source) in sources.enumerated() {
            let repair = streaming && i == sources.count - 1
            if i < pieces.count, pieces[i].repaired == repair, pieces[i].firstIndex == blocks.count,
               pieces[i].source.utf8.elementsEqual(source.utf8) {
                next.append(pieces[i])
            } else {
                var markdown = String(source)
                if repair { markdown = StreamingRepair.repair(markdown) }
                markdown = SourceLines.rulesNotHeadings(markdown)
                next.append(Piece(source: String(source), repaired: repair, firstIndex: blocks.count,
                                  blocks: BlockBuilder.blocks(markdown, firstIndex: blocks.count)))
            }
            blocks.append(contentsOf: next[i].blocks)
        }
        pieces = next
        lastText = text
        lastStreaming = streaming
        lastBlocks = blocks
        return blocks
    }
}

/// Foundation's attributed runs, each tagged with the blocks it sits in, rebuilt as a tree of blocks.
enum BlockBuilder {
    /// Containers deeper than this are flattened into the last one, so hostile nesting stays cheap to draw.
    static let maxDepth = 12

    static func blocks(_ markdown: String, firstIndex: Int) -> [MarkdownBlock] {
        guard !markdown.allSatisfy(\.isWhitespace) else { return [] }
        let options = AttributedString.MarkdownParsingOptions(
            allowsExtendedAttributes: false, interpretedSyntax: .full, failurePolicy: .returnPartiallyParsedIfPossible)
        guard let parsed = try? AttributedString(markdown: markdown, options: options) else {
            return [MarkdownBlock(id: "\(firstIndex)", kind: .paragraph(AttributedString(markdown)))]
        }
        let root = Node(kind: nil, identity: 0)
        var literal = 0
        for run in parsed.runs {
            let span = Span(text: String(parsed[run.range].characters),
                            intent: run.inlinePresentationIntent ?? [], link: run.link, image: run.imageURL)
            guard let intent = run.presentationIntent else {
                // A block of HTML comes with no block at all. It is shown as the text it is.
                literal -= 1
                let node = Node(kind: .paragraph, identity: literal)
                node.spans.append(span)
                root.children.append(node)
                continue
            }
            // Outermost first.
            var path = Array(intent.components.reversed())
            if path.count > maxDepth { path.removeSubrange((maxDepth - 1)..<(path.count - 1)) }
            var node = root
            for component in path {
                if let last = node.children.last, last.identity == component.identity {
                    node = last
                } else {
                    let child = Node(kind: component.kind, identity: component.identity)
                    node.children.append(child)
                    node = child
                }
            }
            node.spans.append(span)
        }
        var out: [MarkdownBlock] = []
        for child in root.children { append(child, to: &out, prefix: "", offset: firstIndex) }
        return out
    }

    private final class Node {
        let kind: PresentationIntent.Kind?
        let identity: Int
        var children: [Node] = []
        var spans: [Span] = []

        init(kind: PresentationIntent.Kind?, identity: Int) {
            self.kind = kind
            self.identity = identity
        }
    }

    /// Adds the blocks a node makes. A block's id is its parent's id and its place among its siblings.
    private static func append(_ node: Node, to out: inout [MarkdownBlock], prefix: String, offset: Int = 0) {
        func add(_ kind: MarkdownBlock.Kind) {
            out.append(MarkdownBlock(id: prefix + String(offset + out.count), kind: kind))
        }
        switch node.kind {
        case .paragraph, .none:
            for part in Inline.parts(node.spans, pictures: true) {
                switch part {
                case .text(let text): add(.paragraph(text))
                case .picture(let url, let alt): add(.image(url: url, alt: alt))
                }
            }
        case .header(let level):
            add(.heading(level: min(max(level, 1), 6), text: Inline.text(node.spans)))
        case .codeBlock(let language):
            var code = node.spans.map(\.text).joined()
            if code.hasSuffix("\n") { code.removeLast() }
            let name = language?.trimmingCharacters(in: .whitespaces)
            add(.code(language: name?.isEmpty == false ? name : nil, text: code))
        case .blockQuote:
            add(.quote(children(of: node, prefix: prefix + String(offset + out.count) + ".")))
        case .orderedList, .unorderedList:
            let id = prefix + String(offset + out.count)
            var items: [MarkdownListItem] = []
            for (i, child) in node.children.enumerated() {
                var number = i + 1
                if case .listItem(let ordinal) = child.kind { number = ordinal }
                let itemID = id + "." + String(i)
                let task = takeTaskBox(child)
                var blocks: [MarkdownBlock] = []
                if case .listItem = child.kind {
                    blocks = children(of: child, prefix: itemID + ".")
                } else {
                    // A flattened tree puts a block straight into the list: it is an item of its own.
                    append(child, to: &blocks, prefix: itemID + ".")
                }
                items.append(MarkdownListItem(id: itemID, number: number, task: task, blocks: blocks))
            }
            add(.list(MarkdownList(ordered: node.kind == .orderedList, items: items)))
        case .thematicBreak:
            add(.rule)
        case .table(let columns):
            let alignments = columns.map { column -> MarkdownTable.Alignment in
                switch column.alignment {
                case .center: .center
                case .right: .trailing
                default: .leading
                }
            }
            var header = [AttributedString](repeating: AttributedString(), count: alignments.count)
            var rows: [[AttributedString]] = []
            for row in node.children {
                var cells = [AttributedString](repeating: AttributedString(), count: alignments.count)
                for cell in row.children {
                    guard case .tableCell(let column) = cell.kind, cells.indices.contains(column) else { continue }
                    cells[column] = Inline.text(cell.spans)
                }
                if row.kind == .tableHeaderRow { header = cells } else { rows.append(cells) }
            }
            add(.table(MarkdownTable(alignments: alignments, header: header, rows: rows)))
        case .listItem, .tableHeaderRow, .tableRow, .tableCell:
            // Out of place (a flattened tree): keep the words.
            for block in children(of: node, prefix: prefix + String(offset + out.count) + ".") { add(block.kind) }
            if !node.spans.isEmpty { add(.paragraph(Inline.text(node.spans))) }
        @unknown default:
            if !node.spans.isEmpty { add(.paragraph(Inline.text(node.spans))) }
        }
    }

    private static func children(of node: Node, prefix: String) -> [MarkdownBlock] {
        var out: [MarkdownBlock] = []
        for child in node.children { append(child, to: &out, prefix: prefix) }
        return out
    }

    /// Foundation leaves a task item's "[ ]" or "[x]" in its text. This takes it off and says which it was.
    private static func takeTaskBox(_ item: Node) -> Bool? {
        guard let first = item.children.first, first.kind == .paragraph, let span = first.spans.first,
              span.intent.isEmpty, span.link == nil, span.image == nil else { return nil }
        for (box, done) in [("[ ]", false), ("[x]", true), ("[X]", true)] {
            if span.text == box, first.spans.count == 1 {
                first.spans.removeFirst()
                return done
            }
            if span.text.hasPrefix(box + " ") {
                first.spans[0].text.removeFirst(4)
                return done
            }
        }
        return nil
    }
}

/// One run of inline text as Foundation gave it.
struct Span {
    var text: String
    var intent: InlinePresentationIntent
    var link: URL?
    var image: URL?
}

enum Inline {
    enum Part {
        case text(AttributedString)
        case picture(URL, alt: String)
    }

    /// The marks the views draw. HTML is not one of them: its text is shown as written.
    private static let drawn: InlinePresentationIntent = [.emphasized, .stronglyEmphasized, .code, .strikethrough]

    /// A paragraph's runs as text, cut where an https picture stands. Any other picture is its alt text.
    static func parts(_ spans: [Span], pictures: Bool) -> [Part] {
        var out: [Part] = []
        var text: [Span] = []
        func flush() {
            let built = build(text)
            if !built.characters.isEmpty { out.append(.text(built)) }
            text = []
        }
        var i = 0
        while i < spans.count {
            var span = spans[i]
            i += 1
            guard let image = span.image else { text.append(span); continue }
            // An alt text with marks in it is several runs of one picture.
            while i < spans.count, spans[i].image == image {
                span.text += spans[i].text
                i += 1
            }
            span.image = nil
            if image.scheme?.lowercased() == "https" {
                if pictures {
                    flush()
                    out.append(.picture(image, alt: span.text))
                    continue
                }
                if span.link == nil { span.link = image }
            }
            if span.text.isEmpty { span.text = "image" }
            text.append(span)
        }
        flush()
        return out
    }

    static func text(_ spans: [Span]) -> AttributedString {
        for part in parts(spans, pictures: false) {
            if case .text(let text) = part { return text }
        }
        return AttributedString()
    }

    private static func build(_ spans: [Span]) -> AttributedString {
        var spans = spans
        // Space left at the edges, where a picture was cut out or a block of HTML ended.
        while let first = spans.first {
            let trimmed = first.text.drop(while: \.isWhitespace)
            if trimmed.isEmpty { spans.removeFirst() } else { spans[0].text = String(trimmed); break }
        }
        while let last = spans.last {
            var text = last.text
            while text.last?.isWhitespace == true { text.removeLast() }
            if text.isEmpty { spans.removeLast() } else { spans[spans.count - 1].text = text; break }
        }
        var out = AttributedString()
        for span in spans {
            var piece = AttributedString(span.text)
            let intent = span.intent.intersection(drawn)
            if !intent.isEmpty { piece.inlinePresentationIntent = intent }
            if let link = span.link.flatMap(MarkdownLinks.allowed) { piece.link = link }
            out.append(piece)
        }
        return out
    }
}
