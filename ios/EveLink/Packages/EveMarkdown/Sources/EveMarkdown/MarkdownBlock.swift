// The block model of a reply: what the parser makes and the views draw. Inline text is an AttributedString that
// carries only Foundation's own marks (bold, italic, strikethrough, code, a link). Foundation only.
import Foundation

/// One block of a reply. Its id is its place in the document ("3", "3.0.1"), so it stays the same while later
/// text streams in.
public struct MarkdownBlock: Identifiable, Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        case paragraph(AttributedString)
        /// Levels 1 to 6.
        case heading(level: Int, text: AttributedString)
        /// A fenced or indented code block, without its last line break.
        case code(language: String?, text: String)
        case quote([MarkdownBlock])
        case list(MarkdownList)
        case table(MarkdownTable)
        /// A picture on an https address. Any other image is its alt text inside a paragraph.
        case image(url: URL, alt: String)
        case rule
    }

    public var id: String
    public var kind: Kind

    public init(id: String, kind: Kind) {
        self.id = id
        self.kind = kind
    }
}

public struct MarkdownList: Equatable, Sendable {
    public var ordered: Bool
    public var items: [MarkdownListItem]

    public init(ordered: Bool, items: [MarkdownListItem]) {
        self.ordered = ordered
        self.items = items
    }
}

public struct MarkdownListItem: Identifiable, Equatable, Sendable {
    public var id: String
    /// The item's number as written (an ordered list may start anywhere); its place in an unordered list.
    public var number: Int
    /// nil for a plain item; false for "- [ ]", true for "- [x]".
    public var task: Bool?
    public var blocks: [MarkdownBlock]

    public init(id: String, number: Int, task: Bool? = nil, blocks: [MarkdownBlock]) {
        self.id = id
        self.number = number
        self.task = task
        self.blocks = blocks
    }
}

public struct MarkdownTable: Equatable, Sendable {
    public enum Alignment: Equatable, Sendable { case leading, center, trailing }

    /// One per column.
    public var alignments: [Alignment]
    public var header: [AttributedString]
    /// Every row has one cell per column.
    public var rows: [[AttributedString]]

    public init(alignments: [Alignment], header: [AttributedString], rows: [[AttributedString]]) {
        self.alignments = alignments
        self.header = header
        self.rows = rows
    }
}

extension MarkdownBlock {
    /// The block's words without any marks, for tests and for search.
    public var plainText: String {
        switch kind {
        case .paragraph(let text), .heading(_, let text): String(text.characters)
        case .code(_, let text): text
        case .quote(let blocks): blocks.map(\.plainText).joined(separator: "\n")
        case .list(let list):
            list.items.map { $0.blocks.map(\.plainText).joined(separator: "\n") }.joined(separator: "\n")
        case .table(let table):
            ([table.header] + table.rows)
                .map { $0.map { String($0.characters) }.joined(separator: "\t") }.joined(separator: "\n")
        case .image(_, let alt): alt
        case .rule: ""
        }
    }
}
