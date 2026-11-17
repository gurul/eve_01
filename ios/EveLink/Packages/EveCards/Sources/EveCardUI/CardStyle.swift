// The look of a card: its few colours, its shapes, and the layout that wraps tags, chips and buttons onto lines.
// Pure black screen in dark mode, the system background in light (ios/EveLink/App/Style.swift); the card is a
// slightly raised surface with a hairline edge. Text uses the system's text styles, so it follows Dynamic Type.
// Behaviour: docs/app-chat.md, "The card engine".
import SwiftUI

enum CardMetrics {
    /// The card's corner.
    static let corner: CGFloat = 20
    /// The corner of a part inside the card: a callout, a field, a picture.
    static let innerCorner: CGFloat = 12
    /// The card's inner margin.
    static let padding: CGFloat = 16
    /// Between a card's children.
    static let gap: CGFloat = 16
    /// The smallest tap target.
    static let tap: CGFloat = 44
}

/// The card's surface: the app's control gray on black, the grouped gray on white.
struct CardSurfaceStyle: ShapeStyle {
    func resolve(in environment: EnvironmentValues) -> Color {
        environment.colorScheme == .dark ? Color(white: 0.11) : Color(red: 0.949, green: 0.949, blue: 0.969)
    }
}

/// The 1 px edge of the card.
struct CardEdgeStyle: ShapeStyle {
    func resolve(in environment: EnvironmentValues) -> Color {
        environment.colorScheme == .dark ? Color.white.opacity(0.09) : Color.black.opacity(0.07)
    }
}

/// A quiet fill inside the card: a field, a tag, a secondary button.
struct CardFillStyle: ShapeStyle {
    func resolve(in environment: EnvironmentValues) -> Color {
        environment.colorScheme == .dark ? Color.white.opacity(0.09) : Color.black.opacity(0.055)
    }
}

/// The selected segment of a tab bar.
struct CardSelectedStyle: ShapeStyle {
    func resolve(in environment: EnvironmentValues) -> Color {
        environment.colorScheme == .dark ? Color(white: 0.26) : Color.white
    }
}

/// A line between rows.
struct CardRuleStyle: ShapeStyle {
    func resolve(in environment: EnvironmentValues) -> Color {
        environment.colorScheme == .dark ? Color.white.opacity(0.12) : Color.black.opacity(0.10)
    }
}

extension ShapeStyle where Self == CardSurfaceStyle { static var cardSurface: CardSurfaceStyle { .init() } }
extension ShapeStyle where Self == CardEdgeStyle { static var cardEdge: CardEdgeStyle { .init() } }
extension ShapeStyle where Self == CardFillStyle { static var cardFill: CardFillStyle { .init() } }
extension ShapeStyle where Self == CardSelectedStyle { static var cardSelected: CardSelectedStyle { .init() } }
extension ShapeStyle where Self == CardRuleStyle { static var cardRule: CardRuleStyle { .init() } }

/// A hairline between two rows.
struct CardRule: View {
    @Environment(\.displayScale) private var scale

    var body: some View {
        Rectangle().fill(.cardRule).frame(height: 1 / max(scale, 1)).accessibilityHidden(true)
    }
}

/// Rows with a hairline between each two.
struct RuledRows<Row: View>: View {
    let count: Int
    @ViewBuilder let row: (Int) -> Row

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(0..<count, id: \.self) { i in
                if i > 0 { CardRule() }
                row(i)
            }
        }
    }
}

/// A row that dims while it is pressed.
struct RowPressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .contentShape(Rectangle())
            .opacity(configuration.isPressed ? 0.5 : 1)
    }
}

/// A card button: filled with the label colour (primary) or with the quiet fill (secondary).
struct CardButtonStyle: ButtonStyle {
    let primary: Bool
    @Environment(\.isEnabled) private var enabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(primary ? AnyShapeStyle(.background) : AnyShapeStyle(.primary))
            .background(primary ? AnyShapeStyle(.primary) : AnyShapeStyle(.cardFill), in: Capsule())
            .contentShape(Capsule())
            .opacity(!enabled ? 0.4 : configuration.isPressed ? 0.6 : 1)
    }
}

/// Children laid out left to right, onto as many lines as they need. A child wider than the line gets the whole
/// line and wraps inside it.
struct FlowLayout: Layout {
    var spacing: CGFloat = 8
    var lineSpacing: CGFloat = 8

    private struct Placement {
        var origin: CGPoint
        var size: CGSize
    }

    private func arrange(_ subviews: Subviews, width: CGFloat) -> (placements: [Placement], size: CGSize) {
        var placements: [Placement] = []
        var x: CGFloat = 0
        var y: CGFloat = 0
        var lineHeight: CGFloat = 0
        var widest: CGFloat = 0
        for subview in subviews {
            var size = subview.sizeThatFits(.unspecified)
            if size.width > width { size = subview.sizeThatFits(ProposedViewSize(width: width, height: nil)) }
            if x > 0 && x + size.width > width {
                x = 0
                y += lineHeight + lineSpacing
                lineHeight = 0
            }
            placements.append(Placement(origin: CGPoint(x: x, y: y), size: size))
            x += size.width + spacing
            lineHeight = max(lineHeight, size.height)
            widest = max(widest, x - spacing)
        }
        return (placements, CGSize(width: widest, height: y + lineHeight))
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        arrange(subviews, width: proposal.width ?? .infinity).size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let placements = arrange(subviews, width: bounds.width).placements
        for (subview, place) in zip(subviews, placements) {
            subview.place(
                at: CGPoint(x: bounds.minX + place.origin.x, y: bounds.minY + place.origin.y),
                proposal: ProposedViewSize(place.size)
            )
        }
    }
}
