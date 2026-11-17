// Draws cards to pictures. Every component is drawn from whatever a model might send, including nothing at all,
// and must come out as a picture rather than a crash. With EVE_CARD_SNAPSHOT_DIR set, the cards are also written
// there as PNG files (light and dark, three widths, mid-stream, failed) for a person to look at:
//
//   EVE_CARD_SNAPSHOT_DIR=/some/folder swift test --filter CardDrawingTests
//
// Behaviour: docs/app-chat.md, "The card engine" and "Using generated cards".
import XCTest
import SwiftUI
import ImageIO
import UniformTypeIdentifiers
import JSONRenderCore
#if canImport(UIKit)
import UIKit
#endif
@testable import EveCardCore
@testable import EveCardUI

@MainActor
final class CardDrawingTests: XCTestCase {
    /// A view as the chat shows it: on the screen's background, 16 pt from each edge.
    private func draw(_ view: some View, width: CGFloat = 393, scheme: ColorScheme = .dark, text: DynamicTypeSize = .large) -> CGImage? {
        let screen = view
            .padding(16)
            .frame(width: width)
            .background(scheme == .dark ? Color.black : Color.white)
            .environment(\.colorScheme, scheme)
            .environment(\.dynamicTypeSize, text)
            .environment(\.cardFadesIn, false)
        #if canImport(UIKit)
        // On an iPhone simulator the card is drawn in a window, so the system's own controls (a text field, a
        // menu) are in the picture. ImageRenderer leaves a placeholder where they are.
        if folder != nil, let image = drawInWindow(screen, width: width) { return image }
        #endif
        let renderer = ImageRenderer(content: screen)
        renderer.scale = 2
        return renderer.cgImage
    }

    #if canImport(UIKit)
    private func drawInWindow(_ view: some View, width: CGFloat) -> CGImage? {
        let host = UIHostingController(rootView: view)
        host.safeAreaRegions = []
        let size = host.sizeThatFits(in: CGSize(width: width, height: .greatestFiniteMagnitude))
        guard size.height > 0, size.height < 20_000 else { return nil }
        let window = UIWindow(frame: CGRect(origin: .zero, size: size))
        window.rootViewController = host
        window.isHidden = false
        host.view.frame = window.bounds
        host.view.layoutIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.15))
        let format = UIGraphicsImageRendererFormat()
        format.scale = 2
        let image = UIGraphicsImageRenderer(size: size, format: format).image { context in
            host.view.layer.render(in: context.cgContext)
        }
        window.isHidden = true
        return image.cgImage
    }
    #endif

    private func card(_ code: String, streaming: Bool = false) -> JsonCardView {
        JsonCardView(stateKey: "draw:\(code.hashValue)", code: code, streaming: streaming, onAsk: { _, _ in }, onOpen: { _ in })
    }

    private var folder: URL? {
        ProcessInfo.processInfo.environment["EVE_CARD_SNAPSHOT_DIR"].map { URL(fileURLWithPath: $0, isDirectory: true) }
    }

    private func save(_ image: CGImage?, _ name: String) throws {
        let image = try XCTUnwrap(image, "\(name) did not draw")
        XCTAssertGreaterThan(image.width, 0)
        XCTAssertGreaterThan(image.height, 0)
        guard let folder else { return }
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let url = folder.appendingPathComponent("\(name).png")
        let out = try XCTUnwrap(CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(out, image, nil)
        XCTAssertTrue(CGImageDestinationFinalize(out))
    }

    func testTheEveryComponentCardDrawsInLightAndDarkAtPhoneWidths() throws {
        let code = try Blocks.named("every component")
        try save(draw(card(code), scheme: .dark), "every-dark-393")
        try save(draw(card(code), scheme: .light), "every-light-393")
        try save(draw(card(code), width: 320, scheme: .dark), "every-dark-320")
        try save(draw(card(code), width: 430, scheme: .light), "every-light-430")
        try save(draw(card(code), scheme: .dark, text: .accessibility1), "every-dark-393-large-text")
    }

    func testTheSixExampleCardsDraw() throws {
        for i in 1...6 {
            let code = try Blocks.named("prompt example \(i)")
            try save(draw(card(code), scheme: .dark), "example-\(i)-dark")
            try save(draw(card(code), scheme: .light), "example-\(i)-light")
        }
    }

    func testAMidStreamCardDrawsWhatHasArrived() throws {
        let lines = try Blocks.named("every component").split(separator: "\n").map(String.init)
        let partial = lines[..<9].joined(separator: "\n") + "\n" + lines[9].prefix(60)
        XCTAssertTrue(inspectCard(partial, streaming: true).renderable)
        try save(draw(card(partial, streaming: true), scheme: .dark), "streaming-dark")
        try save(draw(card(partial, streaming: true), scheme: .light), "streaming-light")
        try save(draw(card(#"{"op":"add","path":"/ro"#, streaming: true), scheme: .dark), "building-dark")
    }

    func testAFailedCardAndTheTextViewDraw() throws {
        try save(draw(card(try Blocks.named("bad: unknown component")), scheme: .dark), "failed-dark")
        try save(draw(card(try Blocks.named("bad: unknown component")), scheme: .light), "failed-light")
        try save(draw(card("That is not a card."), scheme: .dark), "failed-empty-dark")
        let form = try Blocks.named("prompt example 6")
        try save(draw(JsonCardView(stateKey: "draw:text", code: form, showingText: true), scheme: .dark), "show-text-dark")
        let table = try Blocks.named("prompt example 5")
        try save(draw(JsonCardView(stateKey: "draw:text2", code: table, showingText: true), scheme: .light), "show-text-light")
    }

    func testAWideTableAVariantOfEachPartAndALongEverythingDraw() throws {
        let code = [
            line("/root", #""c""#),
            el("c", "Card", #"{"title":"A card whose title is long enough that it has to wrap onto a second line","subtitle":"And a subtitle that is also rather long, to see it wrap as well"}"#,
               ["wide", "info", "ok", "plain", "stacked", "pie", "col", "nums", "tabs", "nested"]),
            el("wide", "Table", #"{"columns":["Plan","Monthly","Yearly","Seats","Storage","Support"],"rows":[["Starter","$8","$80",1,"10 GB","Email"],["Team","$20","$200",10,"200 GB","Chat, within one working day"],["Company","$45","$450",100,"2 TB","Phone"]]}"#),
            el("info", "Callout", #"{"title":"An info callout with no description"}"#),
            el("ok", "Callout", #"{"variant":"success","title":"Saved","description":"A success callout whose description runs long enough to wrap onto a second line of text."}"#),
            el("plain", "Callout", #"{"variant":"neutral","title":"Neutral","description":"With `code` and a [link](https://example.com)."}"#),
            el("stacked", "BarChart", #"{"labels":["Q1","Q2","Q3","Q4"],"series":[{"name":"North","values":[4,6,5,8]},{"name":"South","values":[3,2,6,4]},{"name":"West","values":[1,3,2,5]}],"variant":"stacked"}"#),
            el("pie", "PieChart", #"{"labels":["Rent","Food","Travel","Other"],"values":[50,25,15,10]}"#),
            el("col", "Buttons", #"{"direction":"column"}"#, ["b1", "b2"]),
            el("b1", "Button", #"{"label":"A primary button with a label long enough to need two lines of text"}"#),
            el("b2", "Button", #"{"label":"Secondary","variant":"secondary"}"#),
            el("nums", "List", #"{"items":[{"title":"A plain numbered item"},{"title":"An item with a link and a long title that wraps onto more than one line","subtitle":"example.com","url":"https://example.com/a"}]}"#),
            el("tabs", "Tabs", #"{"tabs":["Overview","Schedule","Budget","Packing list","Notes"]}"#, ["t1", "t2", "t3", "t4", "t5"]),
            el("t1", "Text", #"{"text":"Five tabs do not fit, so the bar scrolls."}"#), el("t2", "Text", #"{"text":"2"}"#),
            el("t3", "Text", #"{"text":"3"}"#), el("t4", "Text", #"{"text":"4"}"#), el("t5", "Text", #"{"text":"5"}"#),
            el("nested", "Card", #"{"title":"A card inside the card","subtitle":"Drawn as an outline"}"#, ["inner"]),
            el("inner", "Text", #"{"text":"Inner text."}"#),
        ].joined(separator: "\n")
        XCTAssertTrue(inspectCard(code).complete, "\(inspectCard(code).issues)")
        try save(draw(card(code), scheme: .dark), "variants-dark-393")
        try save(draw(card(code), width: 320, scheme: .light), "variants-light-320")
    }

    /// Every component with no props, and with props of the wrong shape: each still draws.
    func testEveryComponentDrawsWhateverItIsSent() throws {
        let junk: [String] = [
            "{}",
            #"{"title":5,"subtitle":[],"text":{"a":1},"variant":7,"description":false,"columns":"x","rows":{"a":1},"items":"none","labels":5,"series":"s","values":{"x":1},"src":12,"alt":null,"tags":"t","direction":3,"label":[1],"tabs":"one","sections":null,"placeholder":{},"value":42,"options":"a"}"#,
            #"{"columns":[1,null,{"a":1}],"rows":[null,"x",[null,{"a":1},[1]],[1,2,3,4,5,6,7,8]],"items":[null,5,"x",{"title":null,"image":"x","url":7},{"image":{"src":"ftp://example.com/x"}}],"labels":[null,{"a":1}],"series":[null,{"name":5,"values":"x"},{"name":"s","values":[null,"x",1e308]}],"values":[null,"x",-4],"tags":[null,5,{"a":1}],"tabs":[null,5],"sections":[{}],"options":[null,5,"a","a",""],"value":[null,5],"src":"not a url","variant":"image"}"#,
        ]
        for name in CardRegistry.componentNames {
            for (i, props) in junk.enumerated() {
                // the component under a plain card, and holding children of its own
                let code = [
                    line("/root", #""c""#),
                    el("c", "Card", "{}", ["x"]),
                    el("x", name, props, ["k1", "k2"]),
                    el("k1", "Text", #"{"text":"One"}"#),
                    el("k2", "Button", #"{"label":"Two"}"#),
                ].joined(separator: "\n")
                let program = inspectCard(code, streaming: true)
                XCTAssertTrue(program.renderable, name)
                XCTAssertNotNil(draw(card(code, streaming: true)), "\(name) with props \(i)")
                XCTAssertNotNil(draw(card(code, streaming: false)), "\(name) with props \(i), finished")
            }
        }
    }
}

private func line(_ path: String, _ value: String) -> String {
    "{\"op\":\"add\",\"path\":\"\(path)\",\"value\":\(value)}"
}

private func el(_ key: String, _ type: String, _ props: String, _ children: [String] = []) -> String {
    let kids = children.map { "\"\($0)\"" }.joined(separator: ",")
    return line("/elements/\(key)", "{\"type\":\"\(type)\",\"props\":\(props),\"children\":[\(kids)]}")
}
