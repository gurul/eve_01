// The words of a card as plain text: what Show text shows, and what is recovered from a card that cannot be
// drawn. A port of readableSpec in the web page's specProgram.ts; the two must give the same text for the same
// block (CardParityTests).
// Behaviour: docs/app-chat.md, "Using generated cards".
import Foundation

/// Static, selectable content; never serializes actions, bindings or state.
func readableText(_ spec: NormalSpec?) -> String {
    guard let spec else { return "" }
    var seen = Set<String>()

    func read(_ key: String, _ depth: Int) -> [String] {
        guard let element = spec.elements[key]?.objectValue, !seen.contains(key), depth <= 40 else { return [] }
        seen.insert(key)
        let props = element["props"]?.objectValue ?? CardObject()
        let children = element["children"]?.items ?? []
        func text(_ keys: String...) -> [String] { keys.map { props[$0]?.scalarText ?? "" }.filter { !$0.isEmpty } }
        func kids() -> [String] { children.flatMap { read($0.stringValue ?? "", depth + 1) } }
        func rows(_ key: String) -> [CardObject] { (props[key]?.items ?? []).compactMap(\.objectValue) }
        func pair(_ row: CardObject, _ a: String, _ b: String) -> String {
            [row[a]?.scalarText ?? "", row[b]?.scalarText ?? ""].filter { !$0.isEmpty }.joined(separator: ": ")
        }

        switch element["type"]?.stringValue {
        case "Card": return text("title", "subtitle") + kids()
        case "Text": return text("text")
        case "Callout": return text("title", "description")
        case "Table":
            let columns = (props["columns"]?.items ?? []).map(\.scalarText)
            return (props["rows"]?.items ?? []).map { row in
                row.items.enumerated().map { i, cell in
                    let value = cell.scalarText
                    return "\(i < columns.count ? columns[i] : ""): \(value.isEmpty ? "—" : value)"
                }.joined(separator: " · ")
            }
        case "Facts": return rows("rows").map { pair($0, "label", "value") }
        case "List": return rows("items").map { pair($0, "title", "subtitle") }
        case "Steps": return rows("items").enumerated().map { i, row in "\(i + 1). \(pair(row, "title", "details"))" }
        case "BarChart", "LineChart":
            let labels = props["labels"]?.items ?? []
            return text("xLabel", "yLabel") + rows("series").flatMap { series in
                (series["values"]?.items ?? []).enumerated().map { i, value in
                    [i < labels.count ? labels[i].scalarText : "", series["name"]?.scalarText ?? "", value.scalarText]
                        .filter { !$0.isEmpty }.joined(separator: " · ")
                }
            }
        case "PieChart":
            let labels = props["labels"]?.items ?? []
            return (props["values"]?.items ?? []).enumerated().map { i, value in
                "\(i < labels.count ? labels[i].scalarText : ""): \(value.scalarText)"
            }
        case "Image": return text("alt")
        case "Tags": return [(props["tags"]?.items ?? []).map(\.scalarText).filter { !$0.isEmpty }.joined(separator: " · ")]
        case "Button": return text("label")
        case "FollowUps": return (props["items"]?.items ?? []).map(\.scalarText)
        case "Tabs", "Accordion":
            let names = (props[element["type"]?.stringValue == "Tabs" ? "tabs" : "sections"]?.items ?? []).map(\.scalarText)
            return children.enumerated().flatMap { i, child in
                [i < names.count ? names[i] : ""] + read(child.stringValue ?? "", depth + 1)
            }
        case "TextInput", "Select", "Chips": return text("label")
        default: return kids()
        }
    }

    return read(spec.root, 0).filter { !$0.isEmpty }.joined(separator: "\n\n")
}
