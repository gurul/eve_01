// A json-render block, from the text of a reply to a spec the app can draw: the native twin of the web page's
// specProgram.ts. Nothing in a block is executed: it is data, checked against eve's catalog before the final card
// is shown. A test holds this file to the web page's answers on a shared set of blocks (CardParityTests).
// Behaviour: docs/app-chat.md, "The card engine" and "Using generated cards".
import Foundation
import JSONRenderCore

/// What one block is, and whether it can be drawn.
public struct CardProgram: Sendable {
    public let code: String
    /// What the renderer is given: nil when there is no root card yet.
    public let spec: Spec?
    /// Static, selectable text of the card. Never carries actions, bindings or state.
    public let text: String
    /// A Card root with at least one child: enough to draw while the block streams.
    public let renderable: Bool
    /// Renderable, with no issues: what a finished card must be.
    public let complete: Bool
    /// The card holds something the owner can type or pick, or state of its own.
    public let hasControls: Bool
    /// Why a finished block is not complete. Never shown or logged by the app.
    public let issues: [String]
    /// Elements no children list named, which a finished card shows at its end.
    public let adopted: [String]
}

/// Checks one block. `streaming` says the block is still arriving: its last line may be half written, and an
/// element nothing holds yet is left alone.
public func inspectCard(_ code: String, streaming: Bool = false) -> CardProgram {
    let catalog = CardCatalog.shared
    let (loose, bad) = compileCard(code, streaming: streaming)
    let normal = normalize(loose, adopt: !streaming, catalog: catalog)
    let rootElement = normal.spec.flatMap { $0.elements[$0.root]?.objectValue }
    let rootIsCard = rootElement?["type"]?.stringValue == catalog.root
    let rootChildren = rootElement?["children"]?.items.count ?? 0
    let renderable = normal.spec != nil && rootIsCard && rootChildren > 0

    var issues: [String] = []
    if bad > 0 { issues.append("\(bad) line(s) are not a JSON patch") }
    if normal.spec == nil {
        issues.append("no root element")
    } else if !rootIsCard {
        issues.append("the root must be a \(catalog.root)")
    } else if rootChildren == 0 {
        issues.append("the root card has no children")
    }
    if normal.pruned { issues.append("an element is not in the catalog, or a child is missing") }
    if let spec = normal.spec, !normal.pruned {
        issues += shapeIssues(spec)
        issues += propIssues(spec, catalog: catalog)
        let structure = validateSpecJSON(spec.value.jsonValue)
        if !structure.valid {
            issues += structure.issues.filter { $0.severity != .warning }.prefix(5).map(\.message)
        }
    }

    let hasState = !(normal.spec?.state?.isEmpty ?? true)
    let hasControl = normal.spec.map { spec in
        spec.elements.keys.contains { catalog.controls.contains(spec.elements[$0]?.objectValue?["type"]?.stringValue ?? "") }
    } ?? false
    return CardProgram(
        code: code,
        spec: normal.spec.map(rendererSpec),
        text: readableText(normal.spec),
        renderable: renderable,
        complete: renderable && issues.isEmpty,
        hasControls: hasState || hasControl,
        issues: issues,
        adopted: normal.adopted
    )
}

/// Streaming may show what has arrived so far. A final reply must be whole.
public func selectCard(current: CardProgram, previous: CardProgram?, streaming: Bool) -> CardProgram? {
    if streaming { return current.renderable ? current : previous }
    return current.complete ? current : nil
}

// MARK: - The spec as the renderer takes it

/// A spec whose every element is in the catalog and has props and children.
struct NormalSpec {
    var root: String
    var elements: CardObject
    var state: CardObject?

    var value: CardValue {
        var out = CardObject()
        out["root"] = .string(root)
        out["elements"] = .object(elements)
        if let state { out["state"] = .object(state) }
        return .object(out)
    }
}

/// Every element gets props and children, and a child that is not there (yet), or is not in the catalog, is left
/// out. `pruned` says whether anything was left out.
private func normalize(_ loose: CardValue, adopt: Bool, catalog: CardCatalog) -> (spec: NormalSpec?, pruned: Bool, adopted: [String]) {
    let document = loose.objectValue ?? CardObject()
    let elements = document["elements"]?.objectValue ?? CardObject()
    let root = document["root"]?.stringValue ?? ""
    var pruned = false
    var out = CardObject()
    for key in elements.keys {
        guard var element = elements[key]?.objectValue, let type = element["type"]?.stringValue,
              catalog.props[type] != nil else {
            pruned = true
            continue
        }
        if element["props"]?.objectValue == nil { element["props"] = .object(CardObject()) }
        element["children"] = .array(element["children"]?.items ?? [])
        out[key] = .object(element)
    }
    for key in out.keys {
        guard var element = out[key]?.objectValue else { continue }
        let children = element["children"]?.items ?? []
        let kept = children.filter { child in child.stringValue.map(out.has) ?? false }
        if kept.count != children.count { pruned = true }
        element["children"] = .array(kept)
        out[key] = .object(element)
    }
    guard !root.isEmpty, var rootElement = out[root]?.objectValue else { return (nil, pruned, []) }
    // An element no children list names would never be drawn: the model wrote content and forgot to hang it on the
    // card. In a finished block it goes at the end of the root card, in the order it was written, rather than being
    // dropped without a word. (Not while streaming: a child may arrive before the parent that names it.)
    var adopted: [String] = []
    if adopt {
        var held = Set<String>()
        for key in out.keys {
            for child in out[key]?.objectValue?["children"]?.items ?? [] {
                if let name = child.stringValue { held.insert(name) }
            }
        }
        adopted = out.keys.filter { $0 != root && !held.contains($0) }
        if !adopted.isEmpty {
            rootElement["children"] = .array((rootElement["children"]?.items ?? []) + adopted.map(CardValue.string))
            out[root] = .object(rootElement)
        }
    }
    return (NormalSpec(root: root, elements: liftElementFields(out), state: document["state"]?.objectValue), pruned, adopted)
}

/// json-render's lossless repair (`autoFixSpec`): `visible`, `on`, `repeat` and `watch` written inside props belong
/// on the element, so they are moved there.
private func liftElementFields(_ elements: CardObject) -> CardObject {
    var out = CardObject()
    for key in elements.keys {
        guard var element = elements[key]?.objectValue else { continue }
        if var props = element["props"]?.objectValue {
            for field in ["visible", "on", "repeat", "watch"] {
                guard let value = props[field] else { continue }
                props[field] = nil
                element[field] = value
            }
            element["props"] = .object(props)
        }
        out[key] = .object(element)
    }
    return out
}

// MARK: - The checks a finished card must pass

/// The element fields the catalog's spec schema types beyond type, props and children: `slots` is a map of key lists.
private func shapeIssues(_ spec: NormalSpec) -> [String] {
    var out: [String] = []
    for key in spec.elements.keys {
        guard let slots = spec.elements[key]?.objectValue?["slots"] else { continue }
        let lists = slots.objectValue
        let good = lists.map { object in
            object.keys.allSatisfy { name in
                if case .array(let keys)? = object[name] { return keys.allSatisfy { $0.stringValue != nil } }
                return false
            }
        } ?? false
        if !good { out.append("elements/\(key)/slots: invalid") }
    }
    return Array(out.prefix(5))
}

/// Each element's props against its component's schema. A value that is a dynamic expression ({ "$state": … },
/// { "$bindState": … }) is resolved when the card is drawn, so a mismatch at or under one is not an issue.
private func propIssues(_ spec: NormalSpec, catalog: CardCatalog) -> [String] {
    var out: [String] = []
    for key in spec.elements.keys {
        guard let element = spec.elements[key]?.objectValue, let type = element["type"]?.stringValue,
              let schema = catalog.props[type], let props = element["props"] else { continue }
        for issue in schemaIssues(props, schema) {
            var at: CardValue? = props
            var skip = props.isDynamic
            for step in issue.path where !skip {
                switch (at, step) {
                case (.object(let object)?, .key(let name)): at = object[name]
                case (.array(let array)?, .index(let i)): at = array.indices.contains(i) ? array[i] : nil
                default: at = nil
                }
                if at?.isDynamic == true { skip = true }
            }
            if !skip {
                out.append("\(key) (\(type)) \(issue.path.map(\.text).joined(separator: "/")): \(issue.message)")
            }
        }
    }
    return Array(out.prefix(8))
}

// MARK: - json-render's own model

/// The checked spec as json-render's Swift renderer takes it. An element field the renderer cannot read (a
/// malformed `on`, say) is left off its element, so the rest of the card still draws.
private func rendererSpec(_ spec: NormalSpec) -> Spec {
    var elements: [String: UIElement] = [:]
    for key in spec.elements.keys {
        guard var element = spec.elements[key]?.objectValue else { continue }
        if let decoded = element.decoded() {
            elements[key] = decoded
            continue
        }
        for field in ["on", "watch", "visible", "repeat", "slots"] {
            guard let value = element[field] else { continue }
            var probe = CardObject()
            probe["type"] = .string("")
            probe[field] = value
            if probe.decoded() == nil { element[field] = nil }
        }
        if let decoded = element.decoded() { elements[key] = decoded }
    }
    let state = spec.state.map { CardValue.object($0).jsonValue.objectValue ?? [:] }
    return Spec(root: spec.root, elements: elements, state: state)
}

private extension CardObject {
    func decoded() -> UIElement? {
        CardValue.object(self).jsonValue.decoded(as: UIElement.self)
    }
}
