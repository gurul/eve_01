// What a card's components can ask of the screen around them, passed down the SwiftUI environment: sending a
// message, opening a link, drawing Markdown, and whether the block is still arriving.
// Behaviour: docs/app-chat.md, "Using generated cards".
import SwiftUI
import JSONRenderCore

/// What a card can ask of the chat around it.
struct CardHost: Sendable {
    /// Send `text` to eve as the owner's next message.
    var ask: @MainActor (String) -> Void = { _ in }
    var open: @MainActor (String) -> Void = { _ in }
    /// True while the block is still arriving: buttons wait.
    var streaming = false
    /// The card's state after a change, so what the owner typed or picked can be kept.
    var stateChanged: @MainActor (JSONValue) -> Void = { _ in }
}

extension EnvironmentValues {
    @Entry var cardHost = CardHost()
    /// Draws Markdown inside a card. Nil draws inline Markdown with the system's own reader.
    var cardMarkdown: (@MainActor (String) -> AnyView)? {
        get { self[CardMarkdownKey.self] }
        set { self[CardMarkdownKey.self] = newValue }
    }
    /// How many cards enclose this view: 0 outside a card, 1 inside the root card.
    @Entry var cardDepth = 0
    /// True inside a column of buttons: each button takes the whole width.
    @Entry var cardButtonsFill = false
    /// Whether a part fades in as it first arrives. Off where nothing runs the appearance (a still picture).
    @Entry var cardFadesIn = true
}

private struct CardMarkdownKey: EnvironmentKey {
    static let defaultValue: (@MainActor (String) -> AnyView)? = nil
}

public extension View {
    /// Sets how Markdown is drawn inside cards under this view: a Text's words, a Callout's description, a
    /// step's details. Without it a card draws inline Markdown (bold, italic, code, links) with the system's reader.
    func cardMarkdown(_ render: @escaping @MainActor (String) -> AnyView) -> some View {
        environment(\.cardMarkdown, render)
    }
}

/// Markdown from a card, drawn by the host's renderer when there is one.
struct CardMarkdown: View {
    let text: String
    @Environment(\.cardMarkdown) private var render

    var body: some View {
        if let render {
            render(text)
        } else {
            Text(Self.inline(text)).fixedSize(horizontal: false, vertical: true)
        }
    }

    static func inline(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }
}
