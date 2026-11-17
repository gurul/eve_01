// A reply as it is written: unfinished constructs draw quietly, and finished blocks do not change.
import Foundation
import Testing
@testable import EveMarkdown

private func stream(_ text: String) -> [MarkdownBlock] {
    var parser = MarkdownParser()
    return parser.parse(text, streaming: true)
}

private func words(_ text: String) -> [String] { stream(text).map(\.plainText) }

private let sample = """
# Weekend plan

Here is the plan. It has **bold words**, *italic words*, ~~struck words~~ and `inline code`, a [guide link](https://example.com/guide) and more text after it.

## Packing

1. Tent and bags
2. Stove, with **fuel**

   A second paragraph in the item.
3. Food
   - Oats for breakfast
   - Pasta for *dinner*

- [x] Book the site
- [ ] Check the weather

> A quote with **strong** words.
>
> - a point

```swift
let nights = 2 // **not bold** [not](a link)
print(nights)
```

| Day | Plan | Miles |
|:----|:----:|------:|
| Sat | Hike to the **lake** | 6 |
| Sun | Summit | 9 |

Closing words with a [last link](https://example.com/last).
"""

@Suite struct StreamingTests {
    @Test func aDanglingBoldIsClosedWhereTheTextStops() {
        #expect(words("some **bold and") == ["some bold and"])
        #expect(words("some **bold and ") == ["some bold and"])
        #expect(words("some **") == ["some"])
        #expect(words("some **bold*") == ["some bold"])
        #expect(words("a *b **c") == ["a b c"])
        #expect(words("a ~~gone") == ["a gone"])
        #expect(words("snake_case_name and 2 * 3") == ["snake_case_name and 2 * 3"])
        if case .paragraph(let text)? = stream("some **bold and").first?.kind {
            #expect(text.runs.contains { $0.inlinePresentationIntent == .stronglyEmphasized })
        } else {
            Issue.record("not a paragraph")
        }
    }

    @Test func aHalfWrittenLinkShowsItsWords() {
        #expect(words("see [the guide](") == ["see the guide"])
        #expect(words("see [the guide](https://exam") == ["see the guide"])
        #expect(words("see [the gui") == ["see the gui"])
        #expect(words("see [the guide]") == ["see the guide"])
        #expect(words("see [**the gui") == ["see the gui"])
        #expect(words("see <https://example.com/pa") == ["see https://example.com/pa"])
        #expect(words("a picture ![alt text](https://example.com/a.p") == ["a picture"])
        // Whole links are left as written.
        let whole = stream("see [the guide](https://example.com/a_b) and **mo")
        #expect(whole.map(\.plainText) == ["see the guide and mo"])
        if case .paragraph(let text)? = whole.first?.kind {
            #expect(text.runs.compactMap(\.link) == [URL(string: "https://example.com/a_b")!])
        }
    }

    @Test func anOpenCodeSpanIsCodeSoFar() {
        #expect(words("run `swift bui") == ["run swift bui"])
        #expect(words("run `") == ["run"])
        #expect(words("run `a` then `**b") == ["run a then **b"])
    }

    @Test func anOpenFenceIsACodeBlockSoFar() {
        #expect(stream("```swift\nlet a = 1\nlet b").map(\.kind) == [.code(language: "swift", text: "let a = 1\nlet b")])
        #expect(stream("```swift\nlet a = **1").map(\.kind) == [.code(language: "swift", text: "let a = **1")])
        // The first marks of the closing fence are not code.
        #expect(stream("```\nlet a = 1\n``").map(\.kind) == [.code(language: nil, text: "let a = 1")])
        #expect(stream("text\n\n```").map(\.plainText) == ["text"])
        // Not streaming, the same source is what CommonMark says it is.
        #expect(MarkdownDocument.parse("```\nlet a = 1\n``").map(\.kind) == [.code(language: nil, text: "let a = 1\n``")])
    }

    @Test func aTableWaitsForItsDashesThenHasNoRows() throws {
        #expect(stream("Intro\n\n| Day | Plan |").map(\.plainText) == ["Intro"])
        #expect(stream("Intro\n\n| Day | Plan |\n").map(\.plainText) == ["Intro"])
        #expect(stream("Intro\n\n| Day | Plan |\n|:--").map(\.plainText) == ["Intro"])
        #expect(stream("Intro\n\n| Day | Plan |\n|:---|---:|").map(\.plainText) == ["Intro"])
        let blocks = stream("Intro\n\n| Day | Plan |\n|:---|---:|\n")
        guard case .table(let table) = try #require(blocks.last).kind else { Issue.record("not a table"); return }
        #expect(table.header.map { String($0.characters) } == ["Day", "Plan"])
        #expect(table.rows.isEmpty)
        guard case .table(let grown) = try #require(stream("| Day | Plan |\n|---|---|\n| Sat | **Hi").last).kind else {
            Issue.record("not a table"); return
        }
        #expect(grown.rows.map { $0.map { String($0.characters) } } == [["Sat", "Hi"]])
    }

    @Test func marksWithNothingAfterThemWait() {
        #expect(words("Above\n\n-") == ["Above"])
        #expect(words("Above\n-") == ["Above"])
        #expect(words("Above\n\n1.") == ["Above"])
        #expect(words("Above\n\n##") == ["Above"])
        #expect(words("Above\n\n- [") == ["Above"])
        #expect(words("Above\n\n- [x") == ["Above"])
        #expect(words("Above\n\n> ") == ["Above"])
        #expect(words("Above\n\n**") == ["Above"])
        if case .paragraph? = stream("Above\n-").first?.kind {} else { Issue.record("the text above became a heading") }
    }

    @Test func nothingIsRepairedOnceTheReplyIsWhole() {
        #expect(MarkdownDocument.parse("some **bold and").map(\.plainText) == ["some **bold and"])
        #expect(MarkdownDocument.parse("see [the guide](").map(\.plainText) == ["see [the guide]("])
        // A paragraph that has ended keeps its stray marks even while later text streams.
        #expect(words("2 ** 3 and **more\n\nnext") == ["2 ** 3 and **more", "next"])
    }

    @Test func noRawMarksFlashAtAnyPointOfTheSample() {
        var parser = MarkdownParser()
        var end = sample.startIndex
        while end < sample.endIndex {
            end = sample.index(after: end)
            let prefix = String(sample[..<end])
            for block in parser.parse(prefix, streaming: true) {
                if case .code = block.kind { continue }
                let text = block.plainText
                for mark in ["**", "~~", "`", "](", "[", "]", "|", "#", "- "] {
                    #expect(!text.contains(mark), "\(mark) shows after \(prefix.suffix(30).debugDescription): \(text.debugDescription)")
                }
            }
        }
    }

    @Test func noWordIsDroppedAtAnyPointOfTheSample() {
        var parser = MarkdownParser()
        // Words of the source that must be on screen once written, but for the last one, which may be half a mark.
        func seen(_ text: String) -> [Substring] {
            text.split { !($0.isLetter || $0.isNumber) }.filter { $0.count > 2 }
        }
        let never: Set<Substring> = ["https", "example", "com", "guide", "last", "swift"]
        var end = sample.startIndex
        while end < sample.endIndex {
            end = sample.index(end, offsetBy: 3, limitedBy: sample.endIndex) ?? sample.endIndex
            let prefix = String(sample[..<end])
            let drawn = Set(seen(parser.parse(prefix, streaming: true).map(\.plainText).joined(separator: "\n")))
            // A table's first row waits for its dashes, and the last line may be held: look only above them.
            guard let stable = prefix.range(of: "\n", options: .backwards).map({ prefix[..<$0.lowerBound] }),
                  !stable.contains("| Day") || prefix.contains("-:|\n") else { continue }
            for word in seen(String(stable)) where !never.contains(word) {
                #expect(drawn.contains(word), "\(word) is missing after \(prefix.suffix(30).debugDescription)")
            }
        }
    }

    @Test func finishedBlocksNeverChangeWhileLaterTextArrives() {
        let whole = MarkdownDocument.parse(sample)
        var parser = MarkdownParser()
        var previous: [MarkdownBlock] = []
        var end = sample.startIndex
        while end < sample.endIndex {
            end = sample.index(after: end)
            let blocks = parser.parse(String(sample[..<end]), streaming: true)
            // Only the last block may differ from the one before, and blocks never go away.
            #expect(blocks.count >= previous.count - 1)
            for (old, new) in zip(previous.dropLast(), blocks) { #expect(old == new) }
            // Every block but the last is already what the whole reply has there.
            for (now, final) in zip(blocks.dropLast(), whole) { #expect(now == final) }
            for (now, final) in zip(blocks, whole) { #expect(now.id == final.id) }
            previous = blocks
        }
        #expect(parser.parse(sample) == whole)
    }

    @Test func aGrowingTextParsesTheSameAsAFreshOne() {
        var parser = MarkdownParser()
        var end = sample.startIndex
        while end < sample.endIndex {
            end = sample.index(end, offsetBy: 7, limitedBy: sample.endIndex) ?? sample.endIndex
            let prefix = String(sample[..<end])
            #expect(parser.parse(prefix, streaming: true) == stream(prefix))
            #expect(parser.parse(prefix) == MarkdownDocument.parse(prefix))
        }
    }

    @Test func theSplitterCutsOnlyAtTopLevelBlocks() {
        #expect(SourceLines.pieces("a\n\nb").count == 2)
        #expect(SourceLines.pieces("- a\n\n- b\n\n  more\n\n1. c").count == 1)
        #expect(SourceLines.pieces("```\na\n\nb\n```\n\nc").count == 2)
        #expect(SourceLines.pieces("- a\n\n  ```\n  x\n\ny\n  ```").count == 1)
        #expect(SourceLines.pieces("a\n\n1").count == 1)
        #expect(SourceLines.pieces("a\n\n1x").count == 2)
        #expect(SourceLines.pieces("a\n\n**b**").count == 2)
        #expect(SourceLines.pieces("\n\na").joined() == "\n\na")
    }
}
