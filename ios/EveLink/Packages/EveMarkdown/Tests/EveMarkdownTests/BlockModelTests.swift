// The block model: every construct eve writes, nesting, and input meant to hurt.
import Foundation
import Testing
@testable import EveMarkdown

private func parse(_ text: String) -> [MarkdownBlock] { MarkdownDocument.parse(text) }

/// The marks on the first run whose text is `word`.
private func marks(_ text: AttributedString, _ word: String) -> InlinePresentationIntent? {
    for run in text.runs where String(text[run.range].characters) == word {
        return run.inlinePresentationIntent ?? []
    }
    return nil
}

private func link(_ text: AttributedString, _ word: String) -> URL? {
    for run in text.runs where String(text[run.range].characters) == word { return run.link }
    return nil
}

private func paragraph(_ block: MarkdownBlock?) -> AttributedString {
    if case .paragraph(let text)? = block?.kind { return text }
    Issue.record("not a paragraph: \(String(describing: block))")
    return AttributedString()
}

@Suite struct BlockModelTests {
    @Test func paragraphsAndLineBreaks() {
        let blocks = parse("One line\nsame paragraph.\n\nSecond, hard  \nbreak and\\\nanother.")
        #expect(blocks.map(\.plainText) == ["One line same paragraph.", "Second, hard\nbreak and\nanother."])
        #expect(blocks.map(\.id) == ["0", "1"])
    }

    @Test func headings() {
        let blocks = parse("# One\n## Two\n### Three\n#### Four\n##### Five\n###### Six\n####### Seven")
        let levels = blocks.compactMap { block -> Int? in
            if case .heading(let level, _) = block.kind { return level }
            return nil
        }
        #expect(levels == [1, 2, 3, 4, 5, 6])
        #expect(blocks.last?.plainText == "####### Seven")
        #expect(blocks.first?.plainText == "One")
    }

    @Test func inlineMarks() {
        let text = paragraph(parse("a **bold** b *italic* c ~~gone~~ d `code` e ***both***").first)
        #expect(String(text.characters) == "a bold b italic c gone d code e both")
        #expect(marks(text, "bold") == .stronglyEmphasized)
        #expect(marks(text, "italic") == .emphasized)
        #expect(marks(text, "gone") == .strikethrough)
        #expect(marks(text, "code") == .code)
        #expect(marks(text, "both") == [.emphasized, .stronglyEmphasized])
    }

    @Test func linksAndAutolinks() {
        let text = paragraph(parse("[guide](https://example.com/guide) <https://example.com> mail [me](mailto:a@example.com)").first)
        #expect(link(text, "guide") == URL(string: "https://example.com/guide"))
        #expect(link(text, "https://example.com") == URL(string: "https://example.com"))
        #expect(link(text, "me") == URL(string: "mailto:a@example.com"))
    }

    @Test func onlyAllowedSchemesStayLinks() {
        let text = paragraph(parse("[run](javascript:alert(1)) [file](file:///etc/hosts) [rel](/path) [ok](https://example.com)").first)
        #expect(String(text.characters).contains("run"))
        #expect(text.runs.compactMap(\.link) == [URL(string: "https://example.com")!])
    }

    @Test func unorderedNestedList() throws {
        let blocks = parse("- one\n- two\n  - inner\n    - deepest\n- three")
        guard case .list(let list) = try #require(blocks.first).kind else { Issue.record("not a list"); return }
        #expect(!list.ordered)
        #expect(list.items.count == 3)
        #expect(list.items.map(\.id) == ["0.0", "0.1", "0.2"])
        guard case .list(let inner) = try #require(list.items[1].blocks.last).kind else { Issue.record("no inner list"); return }
        #expect(inner.items.first?.blocks.first?.plainText == "inner")
        guard case .list(let deepest) = try #require(inner.items[0].blocks.last).kind else { Issue.record("no third level"); return }
        #expect(deepest.items.first?.blocks.first?.plainText == "deepest")
    }

    @Test func orderedListKeepsItsNumbersAndParagraphs() throws {
        let blocks = parse("7. seven\n\n   more of seven\n8. eight\n\n   ```\n   code in eight\n   ```")
        #expect(blocks.count == 1)
        guard case .list(let list) = try #require(blocks.first).kind else { Issue.record("not a list"); return }
        #expect(list.ordered)
        #expect(list.items.map(\.number) == [7, 8])
        #expect(list.items[0].blocks.map(\.plainText) == ["seven", "more of seven"])
        #expect(list.items[1].blocks.last?.kind == .code(language: nil, text: "code in eight"))
    }

    @Test func taskLists() throws {
        let blocks = parse("- [ ] open\n- [x] done\n- [X] also done\n- plain\n- [link](https://example.com)")
        guard case .list(let list) = try #require(blocks.first).kind else { Issue.record("not a list"); return }
        #expect(list.items.map(\.task) == [false, true, true, nil, nil])
        #expect(list.items.map { $0.blocks.first?.plainText } == ["open", "done", "also done", "plain", "link"])
    }

    @Test func quotesNestAndHoldLists() throws {
        let blocks = parse("> said\n>\n> - point\n>\n> > deeper")
        guard case .quote(let inner) = try #require(blocks.first).kind else { Issue.record("not a quote"); return }
        #expect(inner.count == 3)
        #expect(inner[0].plainText == "said")
        if case .list(let list) = inner[1].kind { #expect(list.items.count == 1) } else { Issue.record("no list") }
        if case .quote(let deeper) = inner[2].kind { #expect(deeper.first?.plainText == "deeper") } else { Issue.record("no nested quote") }
        #expect(inner.map(\.id) == ["0.0", "0.1", "0.2"])
    }

    @Test func codeBlocks() {
        let blocks = parse("```swift\nlet a = 1\n\n  indented **not bold**\n```\n\n```\nno language\n```\n\n    four spaces\n\n~~~\ntildes\n~~~")
        #expect(blocks.map(\.kind) == [
            .code(language: "swift", text: "let a = 1\n\n  indented **not bold**"),
            .code(language: nil, text: "no language"),
            .code(language: nil, text: "four spaces"),
            .code(language: nil, text: "tildes"),
        ])
    }

    @Test func tablesWithAlignmentAndEmptyCells() throws {
        let blocks = parse("| Name | Count | Note |\n|:-----|:-----:|-----:|\n| a | 1 | **x** |\n| b |  | y |\n| short |")
        guard case .table(let table) = try #require(blocks.first).kind else { Issue.record("not a table"); return }
        #expect(table.alignments == [.leading, .center, .trailing])
        #expect(table.header.map { String($0.characters) } == ["Name", "Count", "Note"])
        #expect(table.rows.map { $0.map { String($0.characters) } } == [["a", "1", "x"], ["b", "", "y"], ["short", "", ""]])
        #expect(marks(table.rows[0][2], "x") == .stronglyEmphasized)
    }

    @Test func aRuleUnderTextIsARuleNotAHeading() {
        #expect(parse("Above\n---\nBelow").map(\.kind) == [
            .paragraph(AttributedString("Above")), .rule, .paragraph(AttributedString("Below")),
        ])
        #expect(parse("a\n\n***\n\nb").map(\.kind)[1] == .rule)
        // Inside code the dashes are code.
        #expect(parse("```\ntext\n---\n```").map(\.kind) == [.code(language: nil, text: "text\n---")])
    }

    @Test func imagesOnHttpsArePicturesAndOthersAreTheirAltText() {
        let blocks = parse("Look: ![a lake](https://example.com/lake.png) nice\n\n![local](lake.png) and ![plain](http://example.com/a.png)")
        #expect(blocks.map(\.kind) == [
            .paragraph(AttributedString("Look:")),
            .image(url: URL(string: "https://example.com/lake.png")!, alt: "a lake"),
            .paragraph(AttributedString("nice")),
            .paragraph(AttributedString("local and plain")),
        ])
        #expect(blocks.map(\.id) == ["0", "1", "2", "3"])
    }

    @Test func escapesAndHTMLAreLiteralText() {
        let blocks = parse("\\*not italic\\* and <b>bold?</b> and 2 &lt; 3\n\n<script>alert(1)</script>\n\n<div onclick=\"x()\">hi</div>")
        #expect(blocks.map(\.plainText) == [
            "*not italic* and <b>bold?</b> and 2 < 3", "<script>alert(1)</script>", "<div onclick=\"x()\">hi</div>",
        ])
        for block in blocks {
            #expect(paragraph(block).runs.allSatisfy { $0.inlinePresentationIntent == nil && $0.link == nil })
        }
    }

    @Test func windowsLineEndings() {
        #expect(parse("# Title\r\n\r\n- a\r\n- b\r\n").count == 2)
    }

    @Test func emptyAndBlankInput() {
        #expect(parse("").isEmpty)
        #expect(parse("\n\n   \n").isEmpty)
    }

    @Test func topLevelIdsCountAcrossPieces() {
        let blocks = parse("# A\n\npara\n\n- x\n\n- y\n\n```\ncode\n\nmore\n```\n\nend")
        #expect(blocks.map(\.id) == ["0", "1", "2", "3", "4"])
        #expect(blocks[3].kind == .code(language: nil, text: "code\n\nmore"))
    }

    // MARK: Hostile input

    private func timed(_ limit: Double, _ work: () -> Void) {
        let start = Date()
        work()
        let took = Date().timeIntervalSince(start)
        #expect(took < limit, "took \(took) s")
    }

    @Test func tenThousandNestedQuotes() {
        timed(2) {
            let blocks = parse(String(repeating: ">", count: 10_000) + " deep")
            #expect(blocks.count <= 1)
        }
        timed(2) { _ = parse(String(repeating: "> ", count: 10_000) + "deep") }
    }

    @Test func fiftyThousandBackticks() {
        timed(2) { _ = parse(String(repeating: "`", count: 50_000)) }
        timed(2) { _ = MarkdownParser().parseStreaming(String(repeating: "`", count: 50_000)) }
        timed(2) { _ = compactLinks("https://example.com " + String(repeating: "`", count: 50_000)) }
    }

    @Test func deeplyNestedListsAreFlattened() {
        let source = (0..<400).map { String(repeating: "  ", count: $0) + "- level \($0)" }.joined(separator: "\n")
        timed(3) {
            let blocks = parse(source)
            func depth(_ block: MarkdownBlock) -> Int {
                switch block.kind {
                case .list(let list): 1 + (list.items.flatMap(\.blocks).map(depth).max() ?? 0)
                case .quote(let inner): 1 + (inner.map(depth).max() ?? 0)
                default: 0
                }
            }
            #expect((blocks.map(depth).max() ?? 0) <= BlockBuilder.maxDepth)
            #expect(blocks.map(\.plainText).joined().contains("level 399"))
        }
    }

    @Test func otherHostileShapes() {
        timed(3) {
            _ = parse(String(repeating: "*a", count: 20_000))
            _ = parse(String(repeating: "[", count: 50_000))
            _ = parse(String(repeating: "|", count: 20_000) + "\n" + String(repeating: "|-", count: 10_000))
            _ = MarkdownParser().parseStreaming(String(repeating: "**[x](", count: 5_000))
            _ = MarkdownParser().parseStreaming(String(repeating: "[", count: 20_000))
        }
    }
}

extension MarkdownParser {
    /// One streaming parse on a fresh or copied parser.
    func parseStreaming(_ text: String) -> [MarkdownBlock] {
        var copy = self
        return copy.parse(text, streaming: true)
    }
}
