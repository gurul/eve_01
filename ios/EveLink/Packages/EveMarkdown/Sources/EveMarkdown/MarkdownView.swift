// eve's markdown drawn as native SwiftUI text (docs/app-chat.md): paragraphs, headings, lists, quotes, code,
// tables, pictures. The source is parsed into blocks once per change and each block is an Equatable view, so a
// reply that streams in re-lays out only the block that grew.
import SwiftUI

/// eve's markdown as native text. Links open through the environment's `openURL` (the host sets it).
public struct MarkdownView: View {
    private let text: String
    private let streaming: Bool
    @State private var memo = ParseMemo()

    public init(_ text: String) {
        self.init(text, streaming: false)
    }

    /// `streaming` is true while the reply is still being written: a half-written construct at its end (an open
    /// "**", a link without its address, a table's first row) is then drawn quietly instead of as raw marks.
    public init(_ text: String, streaming: Bool) {
        self.text = text
        self.streaming = streaming
    }

    public var body: some View {
        MarkdownBlocks(blocks: memo.blocks(text, streaming: streaming))
            .frame(maxWidth: .infinity, alignment: .leading)
            .textSelection(.enabled)
    }
}

/// Keeps the parser, and so the blocks of the unchanged part of the text, across updates of one view.
@MainActor
private final class ParseMemo {
    private var parser = MarkdownParser()

    func blocks(_ text: String, streaming: Bool) -> [MarkdownBlock] {
        parser.parse(text, streaming: streaming)
    }
}

/// Blocks one under the other. Public so a host that parses off the main thread can draw the blocks itself.
public struct MarkdownBlocks: View {
    private let blocks: [MarkdownBlock]
    private let tight: Bool
    @Environment(\.markdownStyle) private var style
    @ScaledMetric private var gap: CGFloat = 12
    @ScaledMetric private var tightGap: CGFloat = 6
    @ScaledMetric private var headingGap: CGFloat = 8

    public init(blocks: [MarkdownBlock]) {
        self.init(blocks: blocks, tight: false)
    }

    init(blocks: [MarkdownBlock], tight: Bool) {
        self.blocks = blocks
        self.tight = tight
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: tight ? tightGap : gap) {
            ForEach(blocks) { block in
                MemoLayout {
                    BlockView(block: block)
                        .equatable()
                        .padding(.top, extraSpace(above: block))
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// A heading sits closer to the text under it than to the text above it.
    private func extraSpace(above block: MarkdownBlock) -> CGFloat {
        guard case .heading(let level, _) = block.kind, level <= 2, block.id != blocks.first?.id else { return 0 }
        return headingGap
    }
}

/// One block. Equal blocks are not drawn again.
struct BlockView: View, Equatable {
    let block: MarkdownBlock
    @Environment(\.markdownStyle) private var style

    nonisolated static func == (a: BlockView, b: BlockView) -> Bool { a.block == b.block }

    var body: some View {
        switch block.kind {
        case .paragraph(let text):
            InlineText(text: text, font: style.font)
        case .heading(let level, let text):
            InlineText(text: text, font: style.headingFont(level))
                .accessibilityAddTraits(.isHeader)
        case .code(_, let text):
            CodeBlock(text: text)
        case .quote(let blocks):
            QuoteBlock(blocks: blocks)
        case .list(let list):
            ListBlock(list: list)
        case .table(let table):
            TableBlock(table: table)
        case .image(let url, let alt):
            PictureBlock(url: url, alt: alt)
        case .rule:
            Divider().padding(.vertical, 4)
        }
    }
}

/// A run of prose: bold, italic and strikethrough as Foundation marked them, code in the monospaced face, links
/// in the accent color.
private struct InlineText: View {
    let text: AttributedString
    let font: Font
    var alignment: TextAlignment = .leading
    @Environment(\.markdownStyle) private var style
    @Environment(\.colorScheme) private var scheme
    @ScaledMetric private var lineGap: CGFloat = 3

    var body: some View {
        Text(drawn)
            .font(font)
            .foregroundStyle(style.color)
            .lineSpacing(lineGap)
            .multilineTextAlignment(alignment)
            .fixedSize(horizontal: false, vertical: true)
    }

    private var drawn: AttributedString {
        var out = text
        for run in out.runs where run.inlinePresentationIntent?.contains(.code) == true {
            out[run.range].font = style.codeFont
            out[run.range].backgroundColor = MarkdownStyle.codeFill(scheme)
        }
        return out
    }
}

/// Code keeps its lines: it scrolls sideways inside its own box and never wraps or clips.
private struct CodeBlock: View {
    let text: String
    @Environment(\.markdownStyle) private var style
    @Environment(\.colorScheme) private var scheme
    @ScaledMetric private var inset: CGFloat = 12

    var body: some View {
        ScrollView(.horizontal) {
            Text(text)
                .font(style.codeBlockFont)
                .foregroundStyle(style.color)
                .fixedSize()
                .padding(inset)
        }
        .scrollIndicators(.hidden)
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(MarkdownStyle.codeFill(scheme), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .accessibilityLabel("Code")
        .accessibilityValue(text)
    }
}

/// A quote: a bar down its left edge and its blocks in the secondary color.
private struct QuoteBlock: View {
    let blocks: [MarkdownBlock]
    @Environment(\.markdownStyle) private var style
    @ScaledMetric private var gap: CGFloat = 12

    var body: some View {
        MarkdownBlocks(blocks: blocks, tight: true)
            .environment(\.markdownStyle, MarkdownStyle(textStyle: style.textStyle, textColor: .secondary))
            .padding(.leading, gap + 3)
            .overlay(alignment: .leading) {
                RoundedRectangle(cornerRadius: 1.5).fill(Color.secondary.opacity(0.4)).frame(width: 3)
            }
    }
}

/// A list: markers in one column, right-aligned, and each item's text hanging beside its marker.
private struct ListBlock: View {
    let list: MarkdownList
    @Environment(\.markdownStyle) private var style
    @Environment(\.markdownListDepth) private var depth
    @ScaledMetric private var itemGap: CGFloat = 6
    @ScaledMetric private var markerGap: CGFloat = 8

    var body: some View {
        VStack(alignment: .leading, spacing: itemGap) {
            ForEach(list.items) { item in
                HangingLayout(gap: markerGap) {
                    marker(item)
                    MarkdownBlocks(blocks: item.blocks, tight: true)
                }
            }
        }
        .environment(\.markdownListDepth, depth + 1)
    }

    /// The widest number of the list, so every marker takes the same column whatever the text size.
    private var widest: String {
        let digits = String(list.items.map(\.number).max() ?? 1).count
        return String(repeating: "0", count: digits) + "."
    }

    @ViewBuilder private func marker(_ item: MarkdownListItem) -> some View {
        if let done = item.task {
            Image(systemName: done ? "checkmark.square.fill" : "square")
                .font(style.font)
                .foregroundStyle(done ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                .accessibilityLabel(done ? "Done" : "Not done")
        } else if list.ordered {
            Text(widest).hidden()
                .overlay(alignment: .trailing) { Text("\(item.number).").fixedSize() }
                .font(style.font.monospacedDigit())
                .foregroundStyle(.secondary)
        } else {
            Text(depth == 0 ? "•" : "◦")
                .font(style.font)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
        }
    }
}

/// A table: a header row in semibold over hairlines. A table that fits sits in the text column, wrapping its
/// cells when it must; one too wide for that scrolls sideways inside itself.
private struct TableBlock: View {
    let table: MarkdownTable
    @Environment(\.markdownStyle) private var style
    @ScaledMetric private var columnGap: CGFloat = 16
    @ScaledMetric private var rowInset: CGFloat = 8
    @ScaledMetric private var columnFloor: CGFloat = 72
    @ScaledMetric private var cellLimit: CGFloat = 240

    var body: some View {
        ViewThatFits(in: .horizontal) {
            grid(.fit)
            ScrollView(.horizontal) { grid(.scroll) }
                .scrollIndicators(.hidden)
                .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        }
    }

    private func grid(_ mode: TableLayout.Mode) -> some View {
        TableLayout(columns: table.alignments.count, alignments: table.alignments, mode: mode,
                    columnGap: columnGap, rowInset: rowInset, columnFloor: columnFloor, cellLimit: cellLimit) {
            ForEach(table.alignments.indices, id: \.self) { column in
                cell(table.header[column], column: column, header: true)
            }
            ForEach(table.rows.indices, id: \.self) { row in
                ForEach(table.alignments.indices, id: \.self) { column in
                    cell(table.rows[row][column], column: column, header: false)
                }
            }
            // One hairline under the header and a fainter one between rows.
            ForEach(0..<max(table.rows.count, 1), id: \.self) { row in
                Divider().opacity(row == 0 ? 1 : 0.5).layoutValue(key: TableLineAfterRow.self, value: row)
            }
        }
    }

    private func cell(_ text: AttributedString, column: Int, header: Bool) -> some View {
        let alignment = table.alignments[column]
        return InlineText(text: text, font: header ? style.tableFont.weight(.semibold) : style.tableFont,
                          alignment: alignment == .center ? .center : alignment == .trailing ? .trailing : .leading)
            .accessibilityAddTraits(header ? .isHeader : [])
    }
}

/// A picture on an https address: rounded, never taller than a third of a screen. A quiet box with its alt text
/// stands in while it loads; a picture that fails is its alt text alone.
private struct PictureBlock: View {
    let url: URL
    let alt: String
    @Environment(\.markdownStyle) private var style
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        AsyncImage(url: url) { phase in
            switch phase {
            case .success(let image):
                image.resizable().scaledToFit()
                    .frame(maxHeight: 280)
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            case .failure:
                Text(alt.isEmpty ? "Picture" : alt)
                    .font(.system(style.stepped(-1)))
                    .foregroundStyle(.secondary)
            default:
                Text(alt.isEmpty ? "Picture" : alt)
                    .font(.system(style.stepped(-2)))
                    .foregroundStyle(.secondary)
                    .padding(12)
                    .frame(maxWidth: .infinity, minHeight: 120)
                    .background(MarkdownStyle.codeFill(scheme), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(alt.isEmpty ? "Picture" : alt)
        .accessibilityAddTraits(.isImage)
    }
}
