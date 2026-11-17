// eve's catalog for json-render's Swift renderer: one SwiftUI view per component in
// ios/EveLink/Web/src/jsonCatalog.ts, and the two actions. json-render decides what is on the card; these decide
// how each part looks. Every component takes whatever the model sent and draws what it can: a missing list is an
// empty list, never a crash. Behaviour: docs/app-chat.md, "The card engine".
import SwiftUI
@preconcurrency import JSONRenderCore
import JSONRenderSwiftUI

/// One element as json-render hands it to a component, for views that live on the main actor.
///
/// json-render builds the tree in a view's body, so a component is always made on the main actor. Its context is
/// not marked Sendable; this wrapper carries it, and an event's work, across that line.
struct CardElement: @unchecked Sendable {
    let context: ComponentRenderContext<AnyView>

    var props: [String: JSONValue] { context.props }
    var children: [IdentifiedChild<AnyView>] { context.identifiedChildren }
    var loading: Bool { context.loading }

    /// A string or a number as text; anything else is "".
    func text(_ name: String) -> String { CardElement.text(props[name]) }

    func list(_ name: String) -> [JSONValue] { props[name]?.arrayValue ?? [] }

    func texts(_ name: String) -> [String] { list(name).map(CardElement.text) }

    static func text(_ value: JSONValue?) -> String {
        switch value {
        case .string(let text)?: return text
        case .number?: return value?.displayString ?? ""
        default: return ""
        }
    }

    /// Runs the actions the block bound to an event.
    func emit(_ event: String) {
        Task { await context.emit(event) }
    }

    /// The state path a prop is bound to with `$bindState`, when it is.
    func binding(_ prop: String) -> String? { context.binding(prop) }

    func state(at path: String) -> JSONValue? { context.stateValue(at: path) }

    func setState(at path: String, to value: JSONValue) { context.setState(at: path, to: value) }
}

private struct Carried<Value>: @unchecked Sendable {
    let value: Value
}

/// A component whose view is made on the main actor, where json-render builds the tree.
private func component<Content: View>(_ make: @escaping @MainActor (CardElement) -> Content) -> ComponentDefinition<AnyView> {
    ComponentDefinition { context in
        let element = CardElement(context: context)
        return MainActor.assumeIsolated {
            Carried(value: AnyView(make(element).modifier(FadeIn(arriving: element.loading))))
        }.value
    }
}

/// The message and link the card's actions carry out. One per card; the registry's handlers hold it.
@MainActor
final class CardActions {
    var ask: @MainActor (String, JSONValue?) -> Void = { _, _ in }
    var open: @MainActor (String) -> Void = { _ in }

    /// The `ask` action: an empty message sends nothing.
    func ask(params: [String: JSONValue]) {
        guard let message = params["message"]?.stringValue, !message.isEmpty else { return }
        ask(message, params["data"])
    }

    /// The `open_url` action.
    func open(params: [String: JSONValue]) {
        guard let url = params["url"]?.stringValue else { return }
        open(url)
    }
}

enum CardRegistry {
    /// The names of the components drawn here. A test holds this to the catalog.
    @MainActor static var componentNames: [String] { Array(components.keys).sorted() }

    /// A registry for one card, whose actions reach `actions`.
    @MainActor
    static func make(actions: CardActions) -> RendererRegistry<AnyView> {
        silenceRendererLog()
        return RendererRegistry(
            components: components,
            actions: [
                "ask": { params in await actions.ask(params: params) },
                "open_url": { params in await actions.open(params: params) },
            ]
        )
    }

    /// json-render prints a warning for a part it skips. A card's contents are the owner's: nothing about them is
    /// printed or logged.
    @MainActor
    private static func silenceRendererLog() {
        JSONRenderLog.handler = nil
    }

    @MainActor private static let components: [String: ComponentDefinition<AnyView>] = [
        "Card": component { CardBlock(element: $0) },
        "Stack": component { StackBlock(element: $0) },
        "Text": component { TextBlock(text: $0.text("text")) },
        "Callout": component { CalloutBlock(variant: $0.text("variant"), title: $0.text("title"), description: $0.text("description")) },
        "Table": component { TableBlock(element: $0) },
        "Facts": component { FactsBlock(element: $0) },
        "List": component { ListBlock(element: $0) },
        "Steps": component { StepsBlock(element: $0) },
        "BarChart": component { BarChartBlock(element: $0) },
        "LineChart": component { LineChartBlock(element: $0) },
        "PieChart": component { PieChartBlock(element: $0) },
        "Image": component { ImageBlock(source: $0.text("src"), alt: $0.text("alt")) },
        "Tags": component { TagsBlock(tags: $0.texts("tags").filter { !$0.isEmpty }) },
        "Buttons": component { ButtonsBlock(element: $0) },
        "Button": component { ButtonBlock(element: $0) },
        "FollowUps": component { FollowUpsBlock(items: $0.texts("items").filter { !$0.isEmpty }) },
        "Tabs": component { TabsBlock(names: $0.texts("tabs"), panels: $0.children) },
        "Accordion": component { AccordionBlock(names: $0.texts("sections"), panels: $0.children) },
        "TextInput": component { TextInputBlock(element: $0) },
        "Select": component { SelectBlock(element: $0) },
        "Chips": component { ChipsBlock(element: $0) },
    ]
}

/// A part fades in as it first arrives while the block streams. A part of a finished card is simply there.
struct FadeIn: ViewModifier {
    let arriving: Bool
    @State private var shown = false
    @Environment(\.cardFadesIn) private var fades
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content
            .opacity(shown || !arriving || !fades || reduceMotion ? 1 : 0)
            .onAppear {
                guard !shown else { return }
                if arriving && fades && !reduceMotion {
                    withAnimation(.easeOut(duration: 0.25)) { shown = true }
                } else {
                    shown = true
                }
            }
    }
}
