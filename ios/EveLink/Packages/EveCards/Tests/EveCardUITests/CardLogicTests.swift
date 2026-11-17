// The card's rules that need no screen: what it shows around the drawing (the web page's JsonCard.test.ts), what
// it keeps of the owner's input, where its actions go, and the picture-list rules.
// Behaviour: docs/app-chat.md, "Using generated cards" and "The card engine".
import XCTest
import SwiftUI
import JSONRenderCore
@testable import EveCardCore
@testable import EveCardUI

struct Blocks {
    struct Case: Decodable { let name: String; let code: String }
    struct File: Decodable { let cases: [Case] }

    static func named(_ name: String) throws -> String {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "card-parity", withExtension: "json"))
        let file = try JSONDecoder().decode(File.self, from: Data(contentsOf: url))
        return try XCTUnwrap(file.cases.first { $0.name == name }, "no fixture named \(name)").code
    }
}

@MainActor
final class CardPresentationTests: XCTestCase {
    private func show(_ code: String, streaming: Bool = false, textMode: Bool = false, session: CardSession = CardSession()) -> CardPresentation {
        let read = session.read(code: code, streaming: streaming)
        return CardPresentation(current: read.current, selected: read.selected, lastGood: read.lastGood, streaming: streaming, textMode: textMode)
    }

    func testAFinishedGoodCardIsDrawnWithShowTextAndNoNotice() throws {
        let show = show(try Blocks.named("prompt example 5"))
        XCTAssertTrue(show.drawsCard)
        XCTAssertFalse(show.hidesCard)
        XCTAssertNil(show.status)
        XCTAssertNil(show.text)
        XCTAssertEqual(show.toggle, "Show text")
        XCTAssertFalse(show.offersReset)
    }

    func testShowTextCoversTheCardWithItsWordsAndOffersTheWayBack() throws {
        let show = show(try Blocks.named("prompt example 5"), textMode: true)
        XCTAssertTrue(show.drawsCard)                       // still there, so its inputs survive
        XCTAssertTrue(show.hidesCard)
        XCTAssertEqual(show.caption, "Text from this reply")
        XCTAssertTrue(try XCTUnwrap(show.text).contains("Compare: Time · Train: 4 h 10 min · Plane: 3 h 30 min"))
        XCTAssertEqual(show.toggle, "Show card")
    }

    func testACardWithControlsOffersResetAndSaysLiveValuesAreLeftOut() throws {
        let form = try Blocks.named("prompt example 6")
        XCTAssertTrue(show(form).offersReset)
        XCTAssertEqual(show(form, textMode: true).caption, "Text from this reply · live values aren’t included")
        XCTAssertFalse(show(form, streaming: true).offersReset)
        XCTAssertNil(show(form, streaming: true).toggle)
    }

    func testAFinishedBlockThatIsNotAWholeCardSaysSoAndShowsTheWordsItCouldRead() throws {
        let show = show(try Blocks.named("bad: unknown component"))
        XCTAssertFalse(show.drawsCard)
        XCTAssertEqual(show.status, "This card couldn’t be displayed. Recovered text may be incomplete.")
        XCTAssertTrue(try XCTUnwrap(show.text).contains("Roast vegetables"))
        XCTAssertNil(show.toggle)
        XCTAssertFalse(show.offersReset)
    }

    func testWithNothingRecoveredItAsksForText() {
        let show = show("That is not a card.")
        XCTAssertEqual(show.status, "This card couldn’t be displayed. Ask Eve to send it as text.")
        XCTAssertNil(show.text)
        XCTAssertNil(show.toggle)
    }

    func testItWaitsWithAStatusLineWhileNothingDrawableHasArrived() {
        let show = show(#"{"op":"add","path":"/ro"#, streaming: true)
        XCTAssertEqual(show.status, "Building your card…")
        XCTAssertFalse(show.drawsCard)
        XCTAssertNil(show.text)
    }

    func testTheLastGoodCardStaysThroughABadChunkAndItsWordsAreRecoveredIfTheBlockEndsBroken() throws {
        let session = CardSession()
        let good = try Blocks.named("patch lines")
        XCTAssertTrue(show(good, streaming: true, session: session).drawsCard)
        // a later chunk that reads as nothing: the card on screen stays
        let broken = show("{", streaming: true, session: session)
        XCTAssertTrue(broken.drawsCard)
        XCTAssertNil(broken.status)
        // the reply ends there: the card is not whole, and the words of the last good one are shown
        let ended = show("{", streaming: false, session: session)
        XCTAssertFalse(ended.drawsCard)
        XCTAssertEqual(ended.status, CardPresentation.failedWithText)
        XCTAssertTrue(try XCTUnwrap(ended.text).contains("Roast vegetables"))
    }

    func testReadingTheSameBlockTwiceChangesNothing() throws {
        let session = CardSession()
        let good = try Blocks.named("patch lines")
        _ = session.read(code: good, streaming: true)
        let a = session.read(code: "{", streaming: true)
        let b = session.read(code: "{", streaming: true)
        XCTAssertEqual(a.selected?.code, good)
        XCTAssertEqual(b.selected?.code, good)
        XCTAssertEqual(b.lastGood?.code, a.lastGood?.code)
    }
}

@MainActor
final class FormStateTests: XCTestCase {
    func testTheCacheKeepsTheHundredMostRecentlyUpdatedCards() {
        let cache = FormStateCache()
        for i in 0..<100 { cache.set("card \(i)", ["/form/city": .string("\(i)")]) }
        cache.set("card 0", ["/form/city": "again"])            // card 0 is now the newest
        cache.set("card 100", ["/form/city": "100"])
        XCTAssertEqual(cache.count, 100)
        XCTAssertNil(cache.get("card 1"))                        // the oldest went
        XCTAssertEqual(cache.get("card 0"), ["/form/city": "again"])
        XCTAssertEqual(cache.get("card 100"), ["/form/city": "100"])
        cache.delete("card 0")
        XCTAssertNil(cache.get("card 0"))
        XCTAssertEqual(cache.count, 99)
    }

    func testEditsAreWhatDiffersFromTheBlocksOwnStateAndGoBackOnTop() {
        let seed: [String: JSONValue] = ["form": ["city": "", "nights": "2", "likes": ["Food"]], "n": 1]
        let now: JSONValue = ["form": ["city": "Porto", "nights": "2", "likes": ["Food", "Views"]], "n": 1, "a/b": true]
        let edits = stateEdits(seed: .object(seed), current: now)
        XCTAssertEqual(edits, ["/form/city": "Porto", "/form/likes": ["Food", "Views"], "/a~1b": true])
        XCTAssertEqual(JSONValue.object(stateWithEdits(seed: seed, edits: edits)), now)
        XCTAssertEqual(stateEdits(seed: .object(seed), current: .object(seed)), [:])
        // a block that had no state yet
        XCTAssertEqual(stateEdits(seed: nil, current: ["form": ["city": "x"]]), ["/form": ["city": "x"]])
    }

    func testWhatTheOwnerTypedSurvivesTheViewBeingMadeAgainAndResetForgetsIt() throws {
        let form = try Blocks.named("prompt example 6")
        let key = "test:\(UUID().uuidString)"
        let first = CardSession()
        _ = first.read(code: form, streaming: false)
        XCTAssertNil(first.initialState(stateKey: key))           // nothing typed: the block's own state is used
        first.stateChanged(["form": ["city": "Porto", "nights": "2"]], stateKey: key)
        // a new session, as when the message scrolls away and back
        let second = CardSession()
        _ = second.read(code: form, streaming: false)
        XCTAssertEqual(second.initialState(stateKey: key), ["form": ["city": "Porto", "nights": "2"]])
        // typing the default back leaves nothing to keep
        second.stateChanged(["form": ["city": "", "nights": "2"]], stateKey: key)
        XCTAssertNil(FormStateCache.shared.get(key))
        // Reset inputs
        first.stateChanged(["form": ["city": "Faro", "nights": "3"]], stateKey: key)
        FormStateCache.shared.delete(key)
        XCTAssertNil(CardSession().initialState(stateKey: key))
    }
}

@MainActor
final class CardActionTests: XCTestCase {
    func testTheCatalogsTwoActionsReachTheHost() async throws {
        let actions = CardActions()
        var asked: [(String, JSONValue?)] = []
        var opened: [String] = []
        actions.ask = { asked.append(($0, $1)) }
        actions.open = { opened.append($0) }
        let registry = CardRegistry.make(actions: actions)
        XCTAssertEqual(Set(registry.actions.keys), Set(CardCatalog.shared.actions))

        try await registry.actions["ask"]?(["message": "Find hotels", "data": ["city": "Porto"]])
        try await registry.actions["ask"]?(["message": ""])                      // an empty message sends nothing
        try await registry.actions["ask"]?(["data": ["city": "Porto"]])
        try await registry.actions["ask"]?(["message": "Plain"])
        XCTAssertEqual(asked.map(\.0), ["Find hotels", "Plain"])
        XCTAssertEqual(asked.first?.1, ["city": "Porto"])
        XCTAssertNil(asked.last?.1)

        try await registry.actions["open_url"]?(["url": "https://example.com/maps/porto"])
        try await registry.actions["open_url"]?(["url": 5])
        XCTAssertEqual(opened, ["https://example.com/maps/porto"])
    }

    func testAButtonsPressSendsTheFormsValuesAsTheyStand() async throws {
        let program = inspectCard(try Blocks.named("prompt example 6"))
        let spec = try XCTUnwrap(program.spec)
        let actions = CardActions()
        var asked: [(String, JSONValue?)] = []
        actions.ask = { asked.append(($0, $1)) }
        let registry = CardRegistry.make(actions: actions)
        let store = StateStore(spec: spec)
        store.set("/form/city", "Porto")                                         // the owner typed a city
        let dispatcher = ActionDispatcher(store: store, handlers: registry.actions)
        let press = try XCTUnwrap(spec.elements["search"]?.on?["press"])
        for binding in press.bindings { await dispatcher.dispatch(binding) }
        XCTAssertEqual(asked.count, 1)
        XCTAssertEqual(asked.first?.0, "Find hotels")
        XCTAssertEqual(asked.first?.1, ["city": "Porto", "nights": "2"])
    }

    func testTheRegistryDrawsExactlyTheCatalogsComponents() {
        XCTAssertEqual(CardRegistry.componentNames, CardCatalog.shared.componentNames)
        XCTAssertEqual(CardRegistry.componentNames.count, 21)
    }
}

final class ListPictureTests: XCTestCase {
    func testPicturesShowWhenAskedForAndAtLeastHalfTheItemsHaveOne() {
        let pic = "https://example.com/1.jpg"
        XCTAssertTrue(showsPictures(variant: "image", sources: [pic, pic, ""]))
        XCTAssertTrue(showsPictures(variant: "image", sources: [pic, ""]))
        XCTAssertFalse(showsPictures(variant: "image", sources: [pic, "", ""]))
        XCTAssertFalse(showsPictures(variant: "image", sources: ["", ""]))
        XCTAssertFalse(showsPictures(variant: "number", sources: [pic, pic]))
        XCTAssertFalse(showsPictures(variant: "", sources: [pic]))
        XCTAssertFalse(showsPictures(variant: "image", sources: []))
    }

    /// The hues the web page's gradientFor gives the same titles (h = (h * 31 + code point) % 360).
    func testATitlesGradientIsTheWebPagesHue() {
        XCTAssertEqual(gradientHue("Jazz night"), WEB_HUES["Jazz night"])
        XCTAssertEqual(gradientHue("Rock night"), WEB_HUES["Rock night"])
        XCTAssertEqual(gradientHue("River walk"), WEB_HUES["River walk"])
        XCTAssertEqual(gradientHue("Café 😀"), WEB_HUES["Café 😀"])
        XCTAssertEqual(gradientHue(""), 0)
        XCTAssertEqual(gradientHue("Jazz night"), gradientHue("Jazz night"))
        XCTAssertNotEqual(gradientHue("Jazz night"), gradientHue("Rock night"))
    }

    func testHslIsTheCssColour() {
        // hsl(0 100% 50%) is red, hsl(120 100% 25%) is #008000, hsl(210 70% 62%) is #5a9ee2
        let close: ((Double, Double, Double), (Double, Double, Double)) -> Bool = {
            abs($0.0 - $1.0) < 0.003 && abs($0.1 - $1.1) < 0.003 && abs($0.2 - $1.2) < 0.003
        }
        XCTAssertTrue(close(hslComponents(0, 1, 0.5), (1, 0, 0)))
        XCTAssertTrue(close(hslComponents(120, 1, 0.25), (0, 0.5, 0)))
        XCTAssertTrue(close(hslComponents(210, 0.7, 0.62), (0x5a / 255.0, 0x9e / 255.0, 0xe2 / 255.0)))
    }

    func testOnlyHttpsPicturesAreLoaded() {
        XCTAssertNotNil(secureURL("https://example.com/a.jpg"))
        XCTAssertNotNil(secureURL("HTTPS://example.com/a.jpg"))
        for bad in ["http://example.com/a.jpg", "file:///etc/hosts", "data:image/png;base64,AAAA", "javascript:alert(1)", "example.com/a.jpg", "", "https://"] {
            XCTAssertNil(secureURL(bad), bad)
        }
    }

    func testChartRowsFollowTheWebPage() {
        let data = chartPoints(
            labels: ["Mon", "Tue", 3],
            series: [["name": "A", "values": [1, "x"]], ["values": [9, 9, 9]], ["name": "B", "values": [4, 5, 6, 7]], "junk"]
        )
        XCTAssertEqual(data.series, ["A", "B"])
        XCTAssertEqual(data.points.map(\.category), ["Mon", "Mon", "Tue", "Tue", "3", "3"])
        XCTAssertEqual(data.points.map(\.value), [1, 4, 0, 5, 0, 6])
        XCTAssertTrue(chartPoints(labels: [], series: [["name": "A", "values": [1]]]).points.isEmpty)
        XCTAssertEqual(pieSlices(labels: ["a", "b", "c"], values: [2, -1]).map(\.value), [2, 0, 0])
    }
}

/// What `gradientFor` in ios/EveLink/Web/src/jsonRegistry.tsx computes for these titles, run in Node.
private let WEB_HUES = ["Jazz night": 55, "Rock night": 189, "River walk": 53, "Café 😀": 73]
