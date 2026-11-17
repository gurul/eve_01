// How a reply's text looks: the text style its prose is set in and its color. A host sets it through the
// environment (the same view draws markdown inside a card at a smaller size). Everything else follows from these:
// headings step up from the base style, code and tables step down.
import SwiftUI

public struct MarkdownStyle: Equatable, Sendable {
    /// The Dynamic Type style of prose. Headings, code and tables are set relative to it.
    public var textStyle: Font.TextStyle
    /// The color of prose. nil is the primary label color.
    public var textColor: Color?

    public init(textStyle: Font.TextStyle = .body, textColor: Color? = nil) {
        self.textStyle = textStyle
        self.textColor = textColor
    }
}

extension EnvironmentValues {
    @Entry public var markdownStyle = MarkdownStyle()
    /// How many lists a block sits inside: a nested list draws a lighter bullet.
    @Entry var markdownListDepth = 0
}

extension View {
    /// Sets how the markdown inside this view is drawn.
    public func markdownStyle(_ style: MarkdownStyle) -> some View {
        environment(\.markdownStyle, style)
    }
}

extension MarkdownStyle {
    /// Text styles from small to large. Headline is body's size, so it steps as body does.
    private static let ladder: [Font.TextStyle] = [
        .caption2, .caption, .footnote, .subheadline, .callout, .body, .title3, .title2, .title, .largeTitle,
    ]

    /// The text style a number of steps above (or below) the base.
    func stepped(_ steps: Int) -> Font.TextStyle {
        let base = textStyle == .headline ? .body : textStyle
        let at = Self.ladder.firstIndex(of: base) ?? 5
        return Self.ladder[min(max(at + steps, 0), Self.ladder.count - 1)]
    }

    var color: Color { textColor ?? .primary }
    var font: Font { .system(textStyle) }
    var codeFont: Font { .system(stepped(-1), design: .monospaced) }
    var codeBlockFont: Font { .system(stepped(-2), design: .monospaced) }
    var tableFont: Font { .system(stepped(-1)) }

    /// Headings are a restrained step up: two sizes for the first level, one for the second, then weight alone.
    func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .system(stepped(2), weight: .bold)
        case 2: .system(stepped(1), weight: .semibold)
        case 3: .system(stepped(0), weight: .semibold)
        default: .system(stepped(-1), weight: .semibold)
        }
    }

    /// A quiet fill behind code: a card's dark gray on pure black, a faint gray on white.
    static func codeFill(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color.white.opacity(0.11) : Color.black.opacity(0.05)
    }
}
