// The components the owner acts on: Button, TextInput, Select and Chips. A control's value lives in the card's
// state when the block binds it with $bindState, so a button can send it along.
// Behaviour: docs/app-chat.md, "Using generated cards".
import SwiftUI
import JSONRenderCore

/// A button. Its press runs the action the block bound to it. It waits while the block is still arriving.
struct ButtonBlock: View {
    let element: CardElement
    @Environment(\.cardHost) private var host
    @Environment(\.cardButtonsFill) private var fill

    var body: some View {
        Button { element.emit("press") } label: {
            Text(element.text("label"))
                .font(.subheadline.weight(.semibold))
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 18)
                .padding(.vertical, 6)
                .frame(maxWidth: fill ? .infinity : nil, minHeight: CardMetrics.tap)
        }
        .buttonStyle(CardButtonStyle(primary: element.text("variant") != "secondary"))
        .disabled(host.streaming)
    }
}

/// A control with its label on a line of its own, above it.
private struct Field<Control: View>: View {
    let label: String
    @ViewBuilder let control: Control

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !label.isEmpty {
                Text(label).font(.footnote).foregroundStyle(.secondary).accessibilityHidden(true)
            }
            control
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private extension View {
    /// The box of a text field or a menu.
    func fieldBox() -> some View {
        padding(.horizontal, 14)
            .frame(maxWidth: .infinity, minHeight: CardMetrics.tap, alignment: .leading)
            .background(.cardFill, in: RoundedRectangle(cornerRadius: CardMetrics.innerCorner, style: .continuous))
    }
}

/// The options of a menu or a chip row: no empty ones, each once, in the order given.
func distinct(_ options: [String]) -> [String] {
    var seen = Set<String>()
    return options.filter { !$0.isEmpty && seen.insert($0).inserted }
}

/// A one-line text field.
struct TextInputBlock: View {
    let element: CardElement
    /// What was typed into a field the block bound to nothing. It goes nowhere, but the field still types.
    @State private var loose: String?

    var body: some View {
        let label = element.text("label")
        let path = element.binding("value")
        let text = Binding<String>(
            get: {
                if let path { return CardElement.text(element.state(at: path)) }
                return loose ?? element.text("value")
            },
            set: { new in
                if let path { element.setState(at: path, to: .string(new)) } else { loose = new }
            }
        )
        Field(label: label) {
            TextField(element.text("placeholder"), text: text)
                .textFieldStyle(.plain)
                .font(.body)
                .fieldBox()
                .accessibilityLabel(label)
        }
    }
}

/// A pick-one menu.
struct SelectBlock: View {
    let element: CardElement
    @State private var loose: String?

    var body: some View {
        let label = element.text("label")
        let path = element.binding("value")
        let options = distinct(element.texts("options"))
        let value = path.map { CardElement.text(element.state(at: $0)) } ?? loose ?? element.text("value")
        let choice = Binding<String>(
            get: { value },
            set: { new in
                if let path { element.setState(at: path, to: .string(new)) } else { loose = new }
            }
        )
        Field(label: label) {
            Menu {
                Picker(label, selection: choice) {
                    ForEach(options, id: \.self) { Text($0).tag($0) }
                }
            } label: {
                HStack(spacing: 8) {
                    Text(value.isEmpty ? "Select…" : value)
                        .font(.body)
                        .foregroundStyle(value.isEmpty ? .secondary : .primary)
                        .lineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Image(systemName: "chevron.up.chevron.down")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .accessibilityHidden(true)
                }
                .fieldBox()
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(label)
            .accessibilityValue(value.isEmpty ? "Nothing selected" : value)
        }
    }
}

/// Pick-several chips. The value is the list of chosen options.
struct ChipsBlock: View {
    let element: CardElement
    @State private var loose: [String]?

    var body: some View {
        let label = element.text("label")
        let path = element.binding("value")
        let options = distinct(element.texts("options"))
        let fixed = element.list("value").compactMap(\.stringValue)
        let chosen = path.map { (element.state(at: $0)?.arrayValue ?? []).compactMap(\.stringValue) } ?? loose ?? fixed
        Field(label: label) {
            FlowLayout(spacing: 8, lineSpacing: 0) {
                ForEach(options, id: \.self) { option in
                    let on = chosen.contains(option)
                    Button {
                        let next = on ? chosen.filter { $0 != option } : chosen + [option]
                        if let path { element.setState(at: path, to: .array(next.map(JSONValue.string))) } else { loose = next }
                    } label: {
                        Text(option)
                            .font(.subheadline.weight(.medium))
                            .lineLimit(1)
                            .padding(.horizontal, 14)
                            .frame(minHeight: 34)
                            .foregroundStyle(on ? AnyShapeStyle(.background) : AnyShapeStyle(.primary))
                            .background(on ? AnyShapeStyle(.primary) : AnyShapeStyle(.cardFill), in: Capsule())
                            // the chip is 34 pt tall; the space above and below it is part of the tap target
                            .padding(.vertical, (CardMetrics.tap - 34) / 2)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(on ? [.isSelected] : [])
                }
            }
            .padding(.vertical, -(CardMetrics.tap - 34) / 2 + 2)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(label)
        }
    }
}
