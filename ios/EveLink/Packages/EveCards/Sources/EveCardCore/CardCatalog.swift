// eve's card catalog, read from Resources/card-catalog.json, and the check of an element's props against its
// component's schema. The resource is written from the web page's catalog (ios/EveLink/Web/src/jsonCatalog.ts) by
// `pnpm gen-swift-catalog`, so nothing about a component's props is written by hand here.
// Behaviour: docs/app-chat.md, "The card engine".
import Foundation

/// The components and actions a card may use.
struct CardCatalog: Sendable {
    /// The root component of every card.
    let root: String
    /// Components that hold what the owner typed or picked.
    let controls: Set<String>
    /// Each component's props, as the JSON Schema zod writes for it.
    let props: [String: CardValue]
    let actions: [String]

    var componentNames: [String] { props.keys.sorted() }

    /// The catalog the package ships with.
    static let shared: CardCatalog = {
        guard let url = Bundle.module.url(forResource: "card-catalog", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let catalog = CardCatalog(json: String(decoding: data, as: UTF8.self)) else {
            // The resource is part of the package: without it no card can be checked, so every card fails closed.
            return CardCatalog(root: "", controls: [], props: [:], actions: [])
        }
        return catalog
    }()

    init(root: String, controls: Set<String>, props: [String: CardValue], actions: [String]) {
        self.root = root
        self.controls = controls
        self.props = props
        self.actions = actions
    }

    init?(json: String) {
        guard let file = parseCardJSON(json)?.objectValue, let root = file["root"]?.stringValue else { return nil }
        var props: [String: CardValue] = [:]
        for component in file["components"]?.items ?? [] {
            guard let name = component.objectValue?["name"]?.stringValue,
                  let schema = component.objectValue?["props"] else { return nil }
            props[name] = schema
        }
        guard props[root] != nil else { return nil }
        self.init(
            root: root,
            controls: Set((file["controls"]?.items ?? []).compactMap(\.stringValue)),
            props: props,
            actions: (file["actions"]?.items ?? []).compactMap { $0.objectValue?["name"]?.stringValue }
        )
    }
}

/// One place a value does not match its schema. `path` holds the keys and indexes that lead to it.
struct SchemaIssue: Equatable {
    var path: [SchemaStep]
    var message: String
}

enum SchemaStep: Equatable {
    case key(String)
    case index(Int)

    var text: String {
        switch self {
        case .key(let key): return key
        case .index(let index): return String(index)
        }
    }
}

/// Checks a value against the part of JSON Schema the catalog uses: type, properties, required, items, enum and
/// anyOf. (`gen-swift-catalog.mjs` refuses to write a schema with any other keyword.)
///
/// Issues land where zod puts them, because the caller forgives an issue at or under a dynamic value by its path:
/// a value that matches no branch of a union is one issue at the union, and a value that may also be null is
/// checked as the type it wraps.
func schemaIssues(_ value: CardValue?, _ schema: CardValue, at path: [SchemaStep] = []) -> [SchemaIssue] {
    guard let schema = schema.objectValue else { return [] }

    if let branches = schema["anyOf"]?.items, !branches.isEmpty {
        let isNull: (CardValue) -> Bool = { $0.objectValue?["type"] == .string("null") }
        let wrapped = branches.filter { !isNull($0) }
        if wrapped.count < branches.count {
            if value == .null { return [] }
            if wrapped.count == 1 { return schemaIssues(value, wrapped[0], at: path) }
        }
        if wrapped.contains(where: { schemaIssues(value, $0, at: path).isEmpty }) { return [] }
        return [SchemaIssue(path: path, message: "matches none of the allowed types")]
    }

    let types: [String]
    switch schema["type"] {
    case .string(let one)?: types = [one]
    case .array(let many)?: types = many.compactMap(\.stringValue)
    default: types = []
    }
    if !types.isEmpty {
        guard let value, types.contains(typeName(value)) else {
            let got = value.map(typeName) ?? "nothing"
            return [SchemaIssue(path: path, message: "expected \(types.joined(separator: " or ")), got \(got)")]
        }
    }
    guard let value else { return [] }

    if let allowed = schema["enum"]?.items, !allowed.contains(value) {
        let names = allowed.map(\.scalarText).joined(separator: ", ")
        return [SchemaIssue(path: path, message: "expected one of \(names)")]
    }

    var issues: [SchemaIssue] = []
    if case .object(let object) = value {
        let properties = schema["properties"]?.objectValue ?? CardObject()
        let required = Set((schema["required"]?.items ?? []).compactMap(\.stringValue))
        for key in properties.keys {
            guard let member = properties[key] else { continue }
            if !object.has(key) && !required.contains(key) { continue }
            issues += schemaIssues(object[key], member, at: path + [.key(key)])
        }
    }
    if case .array(let array) = value, let items = schema["items"] {
        for (i, item) in array.enumerated() {
            issues += schemaIssues(item, items, at: path + [.index(i)])
        }
    }
    return issues
}

private func typeName(_ value: CardValue) -> String {
    switch value {
    case .null: return "null"
    case .bool: return "boolean"
    case .number: return "number"
    case .string: return "string"
    case .array: return "array"
    case .object: return "object"
    }
}
