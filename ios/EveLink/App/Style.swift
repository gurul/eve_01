// Eve Link's few colors and its round buttons (docs/app-chat.md, "The screen"): pure black in dark mode, the system
// background in light, dark gray controls with a 1 px edge. System colors otherwise.
import SwiftUI
import UIKit

enum EveStyle {
    private static func dynamic(dark: UIColor, light: UIColor) -> UIColor {
        UIColor { $0.userInterfaceStyle == .dark ? dark : light }
    }

    /// The screen: pure black in dark mode (not the elevated gray a sheet gets), the system background in light.
    static let background = dynamic(dark: .black, light: .systemBackground)
    /// A control's fill: the top bar's buttons and pill, the composer card.
    static let fill = dynamic(dark: UIColor(white: 0.11, alpha: 1), light: .secondarySystemBackground)
    /// The pill's selected segment.
    static let selected = dynamic(dark: UIColor(white: 0.24, alpha: 1), light: .systemBackground)
    /// The 1 px edge round controls.
    static let edge = dynamic(dark: UIColor(white: 1, alpha: 0.09), light: UIColor(white: 0, alpha: 0.07))
}

extension Color {
    static let eveBackground = Color(uiColor: EveStyle.background)
    static let eveFill = Color(uiColor: EveStyle.fill)
    static let eveSelected = Color(uiColor: EveStyle.selected)
    static let eveEdge = Color(uiColor: EveStyle.edge)
}

/// A round 44 pt top-bar button: dark gray, a hairline edge.
struct RoundButtonStyle: ButtonStyle {
    var size: CGFloat = 44

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(width: size, height: size)
            .background(Circle().fill(Color.eveFill))
            .overlay(Circle().strokeBorder(Color.eveEdge, lineWidth: 1))
            .contentShape(Circle())
            .opacity(configuration.isPressed ? 0.6 : 1)
    }
}

/// The menu button's glyph: two lines, the upper one shorter so the blue dot sits at its end.
struct MenuGlyph: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Capsule().frame(width: 11, height: 2)
            Capsule().frame(width: 16, height: 2)
        }
        .frame(width: 18, alignment: .leading)
        .accessibilityHidden(true)
    }
}
