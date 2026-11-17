// The cost of a reply that streams: a 4,000-character mixed document parsed 200 times with growing prefixes, each
// result compared block by block with the one before, as SwiftUI compares the Equatable block views.
import Foundation
import Testing
@testable import EveMarkdown

@Suite struct PerformanceTests {
    static let document: String = {
        var out = ""
        var section = 0
        while out.count < 4_000 {
            section += 1
            out += """
            ## Section \(section)

            Some **bold** words with a [link](https://example.com/page/\(section)) and `code`, then more words that fill the line out to a fair paragraph length for a chat reply.

            1. First point, with *emphasis*
            2. Second point
               - a nested point
               - another one

            > A quoted line with a little more text in it.

            ```swift
            let value = \(section)
            print(value)
            ```

            | Name | Count |
            |:-----|------:|
            | Alpha | \(section) |
            | Beta | \(section * 2) |


            """
        }
        return String(out.prefix(4_000))
    }()

    @Test func streamingA4000CharacterReply() {
        let text = Self.document
        #expect(text.count == 4_000)
        let steps = 200
        let prefixes = (1...steps).map { String(text.prefix(text.count * $0 / steps)) }

        func run(incremental: Bool) -> (seconds: Double, worst: Double, redrawn: Int) {
            var parser = MarkdownParser()
            var previous: [MarkdownBlock] = []
            var redrawn = 0
            var worst = 0.0
            let start = Date()
            for prefix in prefixes {
                let stepStart = Date()
                if !incremental { parser = MarkdownParser() }
                let blocks = parser.parse(compactLinks(prefix), streaming: true)
                for (i, block) in blocks.enumerated() where i >= previous.count || previous[i] != block { redrawn += 1 }
                previous = blocks
                worst = max(worst, Date().timeIntervalSince(stepStart))
            }
            return (Date().timeIntervalSince(start), worst, redrawn)
        }

        _ = run(incremental: true)   // warm up
        let kept = run(incremental: true)
        let fresh = run(incremental: false)
        let blocks = MarkdownDocument.parse(text).count
        print(String(format: "STREAMING 4000 chars x %d updates: incremental %.1f ms total (%.3f ms mean, %.3f ms worst), "
            + "full re-parse %.1f ms total (%.3f ms mean); %d blocks at the end, %d block redraws (%.2f per update)",
            steps, kept.seconds * 1000, kept.seconds * 1000 / Double(steps), kept.worst * 1000,
            fresh.seconds * 1000, fresh.seconds * 1000 / Double(steps), blocks, kept.redrawn,
            Double(kept.redrawn) / Double(steps)))
        // A frame at 120 Hz is 8.3 ms. The mean update must sit far inside it, even in a debug build.
        #expect(kept.seconds / Double(steps) < 0.004)
        // Only the block being written, and now and then a new one, is drawn again.
        #expect(Double(kept.redrawn) / Double(steps) < 2.5)
    }
}
