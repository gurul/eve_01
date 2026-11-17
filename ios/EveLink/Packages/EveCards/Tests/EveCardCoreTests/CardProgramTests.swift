// The web page's specProgram.test.ts, case for case, against the native checker. Behaviour: docs/app-chat.md,
// "The card engine".
import XCTest
import JSONRenderCore
@testable import EveCardCore

private func json(_ value: Any) -> String {
    let data = try! JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed, .withoutEscapingSlashes])
    return String(decoding: data, as: UTF8.self)
}

/// One patch line. The keys are written in the order the web tests write them, so text edits on a block match.
func line(_ path: String, _ value: String) -> String {
    "{\"op\":\"add\",\"path\":\(json(path)),\"value\":\(value)}"
}

func el(_ key: String, _ type: String, _ props: String, _ children: [String] = [], more: String = "") -> String {
    line("/elements/\(key)", "{\"type\":\(json(type)),\"props\":\(props),\"children\":\(json(children))\(more.isEmpty ? "" : ",\(more)")}")
}

let GOOD = [
    line("/root", "\"card\""),
    el("card", "Card", #"{"title":"Dinner","subtitle":"For two"}"#, ["text", "table"]),
    el("text", "Text", #"{"text":"Roast vegetables"}"#),
    el("table", "Table", #"{"columns":["Dish","Minutes"],"rows":[["Soup",20],["Tart",45]]}"#),
].joined(separator: "\n")

let FORM = [
    line("/root", "\"card\""),
    el("card", "Card", "{}", ["city", "go"]),
    el("city", "TextInput", #"{"label":"City","value":{"$bindState":"/form/city"}}"#),
    line("/state/form", #"{"city":""}"#),
    el("go", "Button", #"{"label":"Search"}"#, more: #""on":{"press":{"action":"ask","params":{"message":"Find hotels","data":{"$state":"/form"}}}}"#),
].joined(separator: "\n")

final class CardProgramTests: XCTestCase {
    func testBuildsASpecFromPatchLinesAndRecoversReadableText() {
        let result = inspectCard(GOOD)
        XCTAssertTrue(result.complete)
        XCTAssertEqual(result.issues, [])
        XCTAssertEqual(result.spec?.root, "card")
        XCTAssertEqual(result.text, "Dinner\n\nFor two\n\nRoast vegetables\n\nDish: Soup · Minutes: 20\n\nDish: Tart · Minutes: 45")
        XCTAssertFalse(result.hasControls)
    }

    func testTakesOneWholeJSONSpecAsWellAsPatchLines() {
        let whole = """
        {
          "root": "card",
          "elements": {
            "card": { "type": "Card", "props": { "title": "Dinner", "subtitle": "For two" }, "children": ["text", "table"] },
            "text": { "type": "Text", "props": { "text": "Roast vegetables" }, "children": [] },
            "table": { "type": "Table", "props": { "columns": ["Dish", "Minutes"], "rows": [["Soup", 20], ["Tart", 45]] }, "children": [] }
          }
        }
        """
        XCTAssertTrue(inspectCard(whole).complete)
        XCTAssertEqual(inspectCard(whole).text, inspectCard(GOOD).text)
    }

    func testAcceptsDynamicValuesWhereAComponentTakesAPlainOneAndKnowsAFormHasControls() {
        let result = inspectCard(FORM)
        XCTAssertEqual(result.issues, [])
        XCTAssertTrue(result.complete)
        XCTAssertTrue(result.hasControls)
        // the readable text never carries bindings, actions or state
        XCTAssertEqual(result.text, "City\n\nSearch")
        // the renderer gets the state and the button's action
        XCTAssertEqual(result.spec?.state, ["form": ["city": ""]])
        XCTAssertNotNil(result.spec?.elements["go"]?.on?["press"])
    }

    func testFillsInWhatALeafMayLeaveOutAndNothingElse() {
        let lean = [
            line("/root", "\"c\""),
            line("/elements/c", #"{"type":"Card","props":{},"children":["t"]}"#),
            line("/elements/t", #"{"type":"Text","props":{"text":"Hi"}}"#),
        ].joined(separator: "\n")
        XCTAssertTrue(inspectCard(lean).complete)
    }

    func testDoesNotCallABrokenBlockComplete() {
        let cases: [(String, String)] = [
            ("a component outside the catalog", GOOD.replacingOccurrences(of: "\"Table\"", with: "\"Grid\"")),
            ("a child that was never added", GOOD.replacingOccurrences(of: #"["text","table"]"#, with: #"["text","table","gone"]"#)),
            ("a cut-off stream", String(GOOD.dropLast(30))),
            ("props the component does not take", GOOD.replacingOccurrences(of: #"[["Soup",20],["Tart",45]]"#, with: #""Soup, 20""#)),
            ("a root that is not a card", GOOD.replacingOccurrences(of: #""type":"Card""#, with: #""type":"Stack""#)),
            ("a card with nothing in it", [line("/root", "\"card\""), el("card", "Card", #"{"title":"Empty"}"#)].joined(separator: "\n")),
            ("prose where a patch belongs", "\(GOOD)\nThat is the plan."),
            ("no block at all", ""),
        ]
        for (name, code) in cases {
            XCTAssertNotEqual(code, GOOD, "the edit did not apply: \(name)")
            let result = inspectCard(code)
            XCTAssertFalse(result.complete, name)
            XCTAssertGreaterThan(result.issues.count, 0, name)
            XCTAssertNil(selectCard(current: result, previous: nil, streaming: false), name)
        }
    }

    func testDrawsWhatHasArrivedWhileStreamingThenHoldsTheLastCardThroughABadUpdate() throws {
        let lastBreak = try XCTUnwrap(GOOD.range(of: "\n", options: .backwards))
        let cut = String(GOOD[..<GOOD.index(lastBreak.upperBound, offsetBy: 39)])   // the last line is half written
        let partial = inspectCard(cut, streaming: true)
        XCTAssertTrue(partial.renderable)
        XCTAssertFalse(partial.complete)
        // the half-written child is left out of what the renderer gets, so nothing dangles
        XCTAssertEqual(partial.spec?.elements["card"]?.children, ["text"])
        XCTAssertEqual(selectCard(current: partial, previous: nil, streaming: true)?.code, partial.code)
        let nothing = inspectCard("{", streaming: true)
        XCTAssertFalse(nothing.renderable)
        XCTAssertEqual(selectCard(current: nothing, previous: partial, streaming: true)?.code, partial.code)
        XCTAssertNil(selectCard(current: nothing, previous: partial, streaming: false))
        XCTAssertEqual(selectCard(current: inspectCard(GOOD), previous: partial, streaming: false)?.complete, true)
    }

    func testShowsAnElementNothingHoldsAtTheEndOfAFinishedCardAndLeavesItOutWhileStreaming() {
        let code = "\(GOOD)\n\(el("extra", "Text", #"{"text":"One more thing"}"#))"
        let done = inspectCard(code)
        XCTAssertTrue(done.complete)
        XCTAssertEqual(done.adopted, ["extra"])
        XCTAssertEqual(done.spec?.elements["card"]?.children, ["text", "table", "extra"])
        XCTAssertTrue(done.text.hasSuffix("One more thing"))
        let live = inspectCard(code, streaming: true)
        XCTAssertEqual(live.adopted, [])
        XCTAssertEqual(live.spec?.elements["card"]?.children, ["text", "table"])
        XCTAssertEqual(inspectCard(GOOD).adopted, [])                 // control: nothing to adopt in a whole card
    }

    func testChecksEveryComponentTheCatalogNames() {
        let catalog = CardCatalog.shared
        XCTAssertEqual(catalog.root, "Card")
        XCTAssertEqual(catalog.componentNames.count, 21)
        XCTAssertTrue(catalog.componentNames.contains("Card"))
        XCTAssertEqual(catalog.controls, ["TextInput", "Select", "Chips"])
        XCTAssertEqual(Set(catalog.actions), ["ask", "open_url"])
    }

    func testAnElementFieldTheRendererCannotReadIsLeftOffAndTheCardStillDraws() {
        let code = [
            line("/root", "\"c\""),
            el("c", "Card", "{}", ["b"]),
            el("b", "Button", #"{"label":"Go"}"#, more: #""on":"press""#),
        ].joined(separator: "\n")
        let result = inspectCard(code)
        XCTAssertTrue(result.renderable)
        XCTAssertEqual(result.spec?.elements["b"]?.props["label"], "Go")
        XCTAssertNil(result.spec?.elements["b"]?.on)
    }
}

final class CardStreamingTests: XCTestCase {
    /// The every-component block, one line at a time, as the app sees a reply arrive: once a card can be drawn it
    /// stays drawable, it only grows, and the finished block is a whole card.
    func testALineByLineStreamNeverGoesBackFromACardToNone() throws {
        let lines = try Fixtures.block("every component").split(separator: "\n").map(String.init)
        XCTAssertGreaterThan(lines.count, 30)
        var shown: CardProgram?
        var sawCard = false
        var elementCount = 0
        for end in 1...lines.count {
            let code = lines[..<end].joined(separator: "\n")
            // a chunk can also end mid-line: the half-written line must not take the card away
            for text in [code, code + "\n" + (end < lines.count ? String(lines[end].prefix(lines[end].count / 2)) : "")] {
                let current = inspectCard(text, streaming: true)
                let next = selectCard(current: current, previous: shown, streaming: true)
                if sawCard {
                    XCTAssertNotNil(next, "the card went away at line \(end)")
                    XCTAssertTrue(current.renderable, "line \(end) is not drawable")
                }
                if let next {
                    sawCard = true
                    XCTAssertTrue(next.renderable)
                    XCTAssertGreaterThanOrEqual(next.spec?.elements.count ?? 0, elementCount, "the card shrank at line \(end)")
                    elementCount = next.spec?.elements.count ?? 0
                }
                shown = next
            }
        }
        XCTAssertTrue(sawCard)
        let done = inspectCard(lines.joined(separator: "\n"))
        XCTAssertTrue(try XCTUnwrap(selectCard(current: done, previous: shown, streaming: false)).complete)
        XCTAssertEqual(done.spec?.elements.count, elementCount)
    }

    func testTheFirstDrawableCardArrivesWithTheFirstChild() throws {
        let lines = try Fixtures.block("every component").split(separator: "\n").map(String.init)
        XCTAssertNil(selectCard(current: inspectCard(lines[0], streaming: true), previous: nil, streaming: true))
        XCTAssertNil(selectCard(current: inspectCard(lines[..<2].joined(separator: "\n"), streaming: true), previous: nil, streaming: true))
        XCTAssertNotNil(selectCard(current: inspectCard(lines[..<3].joined(separator: "\n"), streaming: true), previous: nil, streaming: true))
    }
}

final class CardValueTests: XCTestCase {
    func testNumbersReadAsJavaScriptWritesThem() {
        let cases: [(Double, String)] = [
            (0, "0"), (-0.0, "0"), (20, "20"), (7.5, "7.5"), (-2.25, "-2.25"), (0.1, "0.1"), (100, "100"),
            (1e21, "1e+21"), (1e-7, "1e-7"), (0.000001, "0.000001"), (123456789012, "123456789012"),
            (1e300, "1e+300"), (5e-324, "5e-324"), (12345.678, "12345.678"), (1.5e22, "1.5e+22"), (1e20, "100000000000000000000"),
            (0.30000000000000004, "0.30000000000000004"), (1.25e-9, "1.25e-9"),
        ]
        for (value, text) in cases { XCTAssertEqual(jsNumberText(value), text) }
    }

    func testTheReaderIsAsStrictAsJSONParse() {
        XCTAssertNotNil(parseCardJSON(#" {"a":[1,2,{"b":null}],"c":"é\n😀"} "#))
        for bad in ["{", "{'a':1}", "{\"a\":1,}", "[1,]", "01", "1.", "\"a\nb\"", "{\"a\":1} x", "", "nul", "+1", ".5"] {
            XCTAssertNil(parseCardJSON(bad), bad)
        }
        XCTAssertEqual(parseCardJSON(#""😀""#), .string("😀"))
    }

    func testAnObjectListsItsKeysAsJavaScriptDoes() {
        let object = parseCardJSON(#"{"b":1,"10":2,"a":3,"2":4,"01":5}"#)?.objectValue
        XCTAssertEqual(object?.keys, ["2", "10", "b", "a", "01"])
    }

    func testTrimAndLineSplitFollowJavaScript() {
        XCTAssertEqual(jsTrim("\u{FEFF}\t a b \r\n"), "a b")
        XCTAssertEqual(jsLines("a\r\nb\n\nc"), ["a\r", "b", "", "c"])
    }
}
