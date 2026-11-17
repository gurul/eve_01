// Holds the native card checker to the web page's. Fixtures/card-parity.json is written by the web page's own
// checker (`pnpm gen-card-fixtures` in ios/EveLink/Web, over src/specProgram.ts); every block in it must get the
// same answer from inspectCard. Behaviour: docs/app-chat.md, "The card engine".
import XCTest
@testable import EveCardCore

struct ParityCase: Decodable {
    let name: String
    let code: String
    let streaming: Bool
    let renderable: Bool
    let complete: Bool
    let hasControls: Bool
    let issueCount: Int
    let adopted: [String]
    let text: String
    let elements: [String]?
    let rootChildren: [String]?
}

enum Fixtures {
    static func parity() throws -> [ParityCase] {
        struct File: Decodable { let cases: [ParityCase] }
        let url = try XCTUnwrap(Bundle.module.url(forResource: "card-parity", withExtension: "json", subdirectory: "Fixtures"))
        return try JSONDecoder().decode(File.self, from: Data(contentsOf: url)).cases
    }

    static func block(_ name: String) throws -> String {
        try XCTUnwrap(parity().first { $0.name == name }, "no fixture named \(name)").code
    }
}

final class CardParityTests: XCTestCase {
    func testEveryFixtureBlockGetsTheWebPagesAnswer() throws {
        let cases = try Fixtures.parity()
        XCTAssertGreaterThanOrEqual(cases.count, 50)
        for c in cases {
            let got = inspectCard(c.code, streaming: c.streaming)
            XCTAssertEqual(got.renderable, c.renderable, "renderable: \(c.name)")
            XCTAssertEqual(got.complete, c.complete, "complete: \(c.name) \(got.issues)")
            XCTAssertEqual(got.hasControls, c.hasControls, "hasControls: \(c.name)")
            XCTAssertEqual(got.adopted, c.adopted, "adopted: \(c.name)")
            XCTAssertEqual(got.text, c.text, "text: \(c.name)")
            XCTAssertEqual(got.issues.isEmpty, c.issueCount == 0, "issues: \(c.name) \(got.issues)")
            XCTAssertEqual(got.spec.map { $0.elements.keys.sorted() }, c.elements, "elements: \(c.name)")
            XCTAssertEqual(got.spec.flatMap { $0.elements[$0.root]?.children }, c.rootChildren, "root children: \(c.name)")
            XCTAssertEqual(got.code, c.code)
        }
    }

    /// A wider sweep, for whoever changes the checker: set EVE_CARD_PARITY_FILE to a file of the fixture's shape
    /// (thousands of damaged blocks, say, run through the web page's inspectSpec) and every case must agree.
    func testAnExtraParityFileWhenOneIsGiven() throws {
        guard let path = ProcessInfo.processInfo.environment["EVE_CARD_PARITY_FILE"] else {
            throw XCTSkip("EVE_CARD_PARITY_FILE is not set")
        }
        struct File: Decodable { let cases: [ParityCase] }
        let cases = try JSONDecoder().decode(File.self, from: Data(contentsOf: URL(fileURLWithPath: path))).cases
        var differ = 0
        for c in cases {
            let got = inspectCard(c.code, streaming: c.streaming)
            let same = got.renderable == c.renderable && got.complete == c.complete && got.hasControls == c.hasControls
                && got.adopted == c.adopted && got.text == c.text && got.issues.isEmpty == (c.issueCount == 0)
                && got.spec.map({ $0.elements.keys.sorted() }) == c.elements
                && got.spec.flatMap({ $0.elements[$0.root]?.children }) == c.rootChildren
            if !same {
                differ += 1
                if differ <= 5 { XCTFail("\(c.name) streaming=\(c.streaming): \(got.issues)\n\(c.code)") }
            }
        }
        XCTAssertEqual(differ, 0, "of \(cases.count)")
    }

    func testTheFixtureCoversWhatItClaims() throws {
        let cases = try Fixtures.parity()
        XCTAssertEqual(cases.filter { $0.name.hasPrefix("prompt example") }.count, 6)
        XCTAssertTrue(cases.filter { $0.name.hasPrefix("prompt example") }.allSatisfy(\.complete))
        XCTAssertTrue(cases.filter { $0.name.hasPrefix("bad:") }.allSatisfy { !$0.complete && $0.issueCount > 0 })
        XCTAssertGreaterThanOrEqual(cases.filter { $0.name.hasPrefix("bad:") }.count, 8)
        // one block draws every component in the catalog
        let every = inspectCard(try Fixtures.block("every component"))
        let drawn = Set(try XCTUnwrap(every.spec).elements.values.map(\.type))
        XCTAssertEqual(drawn, Set(CardCatalog.shared.componentNames))
        XCTAssertEqual(drawn.count, 21)
    }
}
