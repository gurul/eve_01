// The components that hold other components: Card, Stack, Tabs, Accordion and Buttons.
// Behaviour: docs/app-chat.md, "The card engine".
import SwiftUI
import JSONRenderCore
import JSONRenderSwiftUI

/// The card: a title, a subtitle and its children, top to bottom, on a raised surface.
struct CardBlock: View {
    let element: CardElement
    @Environment(\.cardDepth) private var depth
    @Environment(\.cardHost) private var host

    var body: some View {
        let title = element.text("title")
        let subtitle = element.text("subtitle")
        let nested = depth > 0
        VStack(alignment: .leading, spacing: CardMetrics.gap) {
            if !title.isEmpty || !subtitle.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    if !title.isEmpty {
                        Text(title)
                            .font(nested ? .headline : .title3.weight(.semibold))
                            .accessibilityAddTraits(.isHeader)
                    }
                    if !subtitle.isEmpty {
                        Text(subtitle).font(.subheadline).foregroundStyle(.secondary)
                    }
                }
                .fixedSize(horizontal: false, vertical: true)
            }
            ForEach(element.children) { $0.content }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(nested ? 12 : CardMetrics.padding)
        .background {
            if !nested { RoundedRectangle(cornerRadius: CardMetrics.corner, style: .continuous).fill(.cardSurface) }
        }
        .overlay {
            RoundedRectangle(cornerRadius: nested ? CardMetrics.innerCorner : CardMetrics.corner, style: .continuous)
                .strokeBorder(.cardEdge, lineWidth: 1)
        }
        .environment(\.cardDepth, depth + 1)
        .modifier(StateReport(state: nested ? nil : element.state(at: ""), report: host.stateChanged))
    }
}

/// Tells the card's host when the card's state changes, so what the owner typed or picked can be kept.
private struct StateReport: ViewModifier {
    let state: JSONValue?
    let report: @MainActor (JSONValue) -> Void

    func body(content: Content) -> some View {
        content.onChange(of: state) { _, now in
            if let now { report(now) }
        }
    }
}

/// A few children in a column, or in a row that wraps.
struct StackBlock: View {
    let element: CardElement

    var body: some View {
        if element.text("direction") == "row" {
            FlowLayout(spacing: 12, lineSpacing: 12) {
                ForEach(element.children) { $0.content }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(element.children) { $0.content }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Tabs: a segmented bar of the labels, and the chosen tab's child below it.
struct TabsBlock: View {
    let names: [String]
    let panels: [IdentifiedChild<AnyView>]
    @State private var selected = 0
    @Namespace private var bar
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private func name(_ i: Int) -> String {
        i < names.count && !names[i].isEmpty ? names[i] : "Tab \(i + 1)"
    }

    var body: some View {
        if !panels.isEmpty {
            let current = min(selected, panels.count - 1)
            VStack(alignment: .leading, spacing: 12) {
                ViewThatFits(in: .horizontal) {
                    segments(current: current, fill: true)
                    ScrollView(.horizontal) { segments(current: current, fill: false) }
                        .scrollIndicators(.hidden)
                        .clipShape(Capsule())
                }
                panels[current].content
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .id(panels[current].id)
            }
        }
    }

    private func segments(current: Int, fill: Bool) -> some View {
        HStack(spacing: 2) {
            ForEach(panels.indices, id: \.self) { i in
                Button {
                    withAnimation(reduceMotion ? nil : .snappy(duration: 0.22)) { selected = i }
                } label: {
                    Text(name(i))
                        .font(.subheadline.weight(i == current ? .semibold : .regular))
                        .lineLimit(1)
                        .padding(.horizontal, 14)
                        .frame(maxWidth: fill ? .infinity : nil, minHeight: CardMetrics.tap - 6)
                        .background {
                            if i == current {
                                Capsule().fill(.cardSelected)
                                    .shadow(color: .black.opacity(0.08), radius: 1.5, y: 0.5)
                                    .matchedGeometryEffect(id: "selected", in: bar)
                            }
                        }
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(i == current ? [.isSelected] : [])
                .accessibilityHint("Tab \(i + 1) of \(panels.count)")
            }
        }
        .padding(3)
        .background(.cardFill, in: Capsule())
    }
}

/// Collapsible sections, one open at a time. The first starts open.
struct AccordionBlock: View {
    let names: [String]
    let panels: [IdentifiedChild<AnyView>]
    @State private var open: Int? = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private func name(_ i: Int) -> String {
        i < names.count && !names[i].isEmpty ? names[i] : "Section \(i + 1)"
    }

    var body: some View {
        if !panels.isEmpty {
            VStack(alignment: .leading, spacing: 0) {
                CardRule()
                ForEach(panels.indices, id: \.self) { i in
                    Button {
                        withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.2)) { open = open == i ? nil : i }
                    } label: {
                        HStack(spacing: 12) {
                            Text(name(i))
                                .font(.body.weight(.medium))
                                .multilineTextAlignment(.leading)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Image(systemName: "chevron.down")
                                .font(.footnote.weight(.semibold))
                                .foregroundStyle(.secondary)
                                .rotationEffect(.degrees(open == i ? 180 : 0))
                                .accessibilityHidden(true)
                        }
                        .frame(minHeight: CardMetrics.tap)
                    }
                    .buttonStyle(RowPressStyle())
                    .accessibilityValue(open == i ? "Expanded" : "Collapsed")
                    if open == i {
                        panels[i].content
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.top, 2)
                            .padding(.bottom, 14)
                            .transition(.opacity)
                    }
                    CardRule()
                }
            }
        }
    }
}

/// A group of buttons: side by side, wrapping onto lines, or stacked at full width.
struct ButtonsBlock: View {
    let element: CardElement

    var body: some View {
        if element.text("direction") == "column" {
            VStack(spacing: 8) {
                ForEach(element.children) { $0.content }
            }
            .frame(maxWidth: .infinity)
            .environment(\.cardButtonsFill, true)
        } else {
            FlowLayout(spacing: 8, lineSpacing: 8) {
                ForEach(element.children) { $0.content }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
