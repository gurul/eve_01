// The Eve Show-and-Tell brand, shared by the helper app and the widget extension.
//
// Paper ground, ink text, cut-paper cards: a 2pt ink border and a solid offset
// shadow in one brand colour (never a blur). Grandstander for headings, Andika
// (a typeface made for beginning readers) for body text, Gochi Hand for the
// handwritten kickers, VT323 only on the robot's screen.
//
// The fonts ship in Shared/Fonts (SIL OFL, licence files beside them) and are
// registered for this process by `BrandFonts.register()`. Every `BrandFont`
// helper falls back to a rounded system face when a font did not load, so a
// failed registration costs looks, never legibility.

import AppKit
import CoreText
import SwiftUI
import WidgetKit

// MARK: - Fonts

enum BrandFonts {
    /// Runs once per process for the bundled fonts, however many times it is called.
    private static let bundleOnce: Void = {
        registerAll(Bundle.main.urls(forResourcesWithExtension: "ttf", subdirectory: nil) ?? [])
    }()

    /// Registers the brand fonts for this process. With no directory it reads the
    /// app or extension bundle's Resources; a directory is for tools such as a
    /// render harness that run outside a bundle.
    static func register(in directory: URL? = nil) {
        guard let directory else { _ = bundleOnce; return }
        let urls = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
        registerAll(urls.filter { $0.pathExtension.lowercased() == "ttf" })
    }

    private static func registerAll(_ urls: [URL]) {
        for url in urls {
            // An "already registered" error is harmless: the face is usable either way.
            CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        }
    }

    static func available(_ postScriptName: String) -> Bool {
        NSFont(name: postScriptName, size: 12) != nil
    }
}

enum BrandFont {
    /// Grandstander, for headings.
    static func display(_ size: CGFloat, black: Bool = true) -> Font {
        let name = black ? "Grandstander-Black" : "Grandstander-Bold"
        return BrandFonts.available(name)
            ? .custom(name, fixedSize: size)
            : .system(size: size, weight: black ? .black : .bold, design: .rounded)
    }

    /// Andika, for everything a learner reads.
    static func body(_ size: CGFloat, bold: Bool = false) -> Font {
        let name = bold ? "Andika-Bold" : "Andika"
        return BrandFonts.available(name)
            ? .custom(name, fixedSize: size)
            : .system(size: size, weight: bold ? .semibold : .regular, design: .rounded)
    }

    /// Gochi Hand, for handwritten kickers and speech lines.
    static func hand(_ size: CGFloat) -> Font {
        BrandFonts.available("GochiHand-Regular")
            ? .custom("GochiHand-Regular", fixedSize: size)
            : .system(size: size, weight: .medium, design: .rounded).italic()
    }

    /// VT323, only for text on the robot's screen.
    static func screen(_ size: CGFloat) -> Font {
        BrandFonts.available("VT323-Regular")
            ? .custom("VT323-Regular", fixedSize: size)
            : .system(size: size, design: .monospaced)
    }
}

// MARK: - Colours

extension Color {
    init(hex: UInt32, opacity: Double = 1) {
        self.init(.sRGB,
                  red: Double((hex >> 16) & 0xFF) / 255,
                  green: Double((hex >> 8) & 0xFF) / 255,
                  blue: Double(hex & 0xFF) / 255,
                  opacity: opacity)
    }

    static let brandPaper = Color(hex: 0xFBF5EC)
    static let brandSheet = Color(hex: 0xFFFDF8)
    static let brandInk = Color(hex: 0x1F3A78)
    static let brandInkSoft = Color(hex: 0x3E4F82)
    static let brandPink = Color(hex: 0xE68AAE)
    static let brandPinkSoft = Color(hex: 0xF4C3D4)
    static let brandTeal = Color(hex: 0x79C6B2)
    static let brandSun = Color(hex: 0xF1C85B)
    static let brandSky = Color(hex: 0x7FA2DE)
    static let brandDesk = Color(hex: 0xE9E1D3)
    static let brandScreen = Color(hex: 0x1B2350)
    static let brandEye = Color(hex: 0xFF74D4)
    /// Sun at 35% over paper, pre-mixed so it stays opaque: a see-through card fill
    /// would let its own offset shadow show through.
    static let brandSunWash = Color(hex: 0xF8E5B9)
}

/// Brand colours for a feeling, used for card shadows and pills. The robot's LED
/// colours stay in `MoodColor.rgb`, which mirrors the firmware.
enum BrandMood {
    static func color(_ label: String?) -> Color {
        switch label {
        case "happy": .brandSun
        case "curious": .brandTeal
        case "calm", "lonely": .brandSky
        case "surprised", "startled": .brandPink
        case "affection": .brandPinkSoft
        case "bored": .brandDesk
        // An unknown label still needs a visible cut-paper edge; sheet on paper disappears.
        default: .brandSky
        }
    }
}

// MARK: - Flat (vibrant / accented) rendering

/// On a macOS desktop a widget is drawn "vibrant" whenever an app window is in
/// front, and "accented" with a tinted accent. The system then removes the paper
/// background and draws every colour by its brightness, so dark ink would go faint
/// on the dimmed desktop. In those modes the brand drops its fills and shadows and
/// draws text and lines in white, which the system shows at full strength.
enum BrandRender {
    static func isFlat(_ environment: EnvironmentValues) -> Bool {
        environment.brandFlatRendering ?? (environment.widgetRenderingMode != .fullColor)
    }
}

private struct BrandFlatRenderingKey: EnvironmentKey {
    static let defaultValue: Bool? = nil
}

extension EnvironmentValues {
    /// Forces flat (true) or full-colour (false) brand rendering. Nil, the default,
    /// follows `widgetRenderingMode`. For render harnesses: the real rendering mode
    /// cannot be set from outside WidgetKit.
    var brandFlatRendering: Bool? {
        get { self[BrandFlatRenderingKey.self] }
        set { self[BrandFlatRenderingKey.self] = newValue }
    }
}

/// A brand colour that turns into its flat stand-in when the widget is not full colour.
/// The app always renders full colour, so there it is the plain brand colour.
struct BrandStyle: ShapeStyle {
    let full: Color
    let flat: Color

    func resolve(in environment: EnvironmentValues) -> Color {
        BrandRender.isFlat(environment) ? flat : full
    }

    /// Ink for text, borders and glyphs.
    static let ink = BrandStyle(full: .brandInk, flat: .white)
    /// Soft ink for times, captions and kickers.
    static let inkSoft = BrandStyle(full: .brandInkSoft, flat: .white.opacity(0.72))
    /// Pink for a small accent glyph.
    static let pink = BrandStyle(full: .brandPink, flat: .white.opacity(0.72))

    /// A fill that disappears in flat rendering, so text over it stays readable.
    static func fill(_ color: Color) -> BrandStyle { BrandStyle(full: color, flat: .clear) }
}

// MARK: - Cut-paper pieces

/// A sheet of paper with an ink border and a solid offset shadow in one colour.
struct PaperCard: ViewModifier {
    var shadow: Color
    var offset: CGFloat = 4
    var radius: CGFloat = 5
    var fill: Color = .brandSheet
    var lineWidth: CGFloat = 2

    func body(content: Content) -> some View {
        // In flat rendering a fill or shadow would read as a smudge: only the
        // border stays, and it turns white with the text (see BrandRender).
        content
            .background(RoundedRectangle(cornerRadius: radius).fill(BrandStyle.fill(fill)))
            .overlay(RoundedRectangle(cornerRadius: radius).strokeBorder(BrandStyle.ink, lineWidth: lineWidth))
            .background(
                RoundedRectangle(cornerRadius: radius)
                    .fill(BrandStyle.fill(shadow))
                    .offset(x: offset, y: offset)
            )
    }
}

extension View {
    func paperCard(shadow: Color, offset: CGFloat = 4, radius: CGFloat = 5,
                   fill: Color = .brandSheet, lineWidth: CGFloat = 2) -> some View {
        modifier(PaperCard(shadow: shadow, offset: offset, radius: radius, fill: fill, lineWidth: lineWidth))
    }
}

/// A small tag: bold capitals in a capsule with an ink border.
struct BrandPill: View {
    let text: String
    var fill: Color = .brandSheet
    var size: CGFloat = 10.5

    init(_ text: String, fill: Color = .brandSheet, size: CGFloat = 10.5) {
        self.text = text
        self.fill = fill
        self.size = size
    }

    var body: some View {
        Text(text.uppercased())
            .font(BrandFont.body(size, bold: true))
            .tracking(0.6)
            .foregroundStyle(BrandStyle.ink)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: true)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(Capsule().fill(BrandStyle.fill(fill)))
            .overlay(Capsule().strokeBorder(BrandStyle.ink, lineWidth: 1.5))
    }
}

/// A Grandstander heading with the pink-soft offset copy behind it (the deck's h2).
struct InkHeading: View {
    let text: String
    let size: CGFloat
    @Environment(\.self) private var environment

    init(_ text: String, size: CGFloat) {
        self.text = text
        self.size = size
    }

    var body: some View {
        ZStack(alignment: .topLeading) {
            // The light pink copy would be the brightest layer in flat rendering and
            // make the heading look doubled, so it is drawn only in full colour.
            if !BrandRender.isFlat(environment) {
                Text(text)
                    .foregroundStyle(Color.brandPinkSoft)
                    .offset(x: size * 0.06, y: size * 0.06)
                    .accessibilityHidden(true)
            }
            Text(text)
                .foregroundStyle(BrandStyle.ink)
                .widgetAccentable()
        }
        .font(BrandFont.display(size))
        .lineLimit(1)
        .minimumScaleFactor(0.75)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

// MARK: - The robot

/// eve, drawn from the deck's SVG (viewBox 200 x 240). Static on purpose: no
/// rotation, no animation, and no pixel snapping, which all looked glitchy.
/// `compact` draws only the head bezel, the screen and the eyes.
struct RobotFace: View {
    let size: CGFloat
    var line: String? = nil
    var compact: Bool = false

    init(size: CGFloat, line: String? = nil, compact: Bool = false) {
        self.size = size
        self.line = line
        self.compact = compact
    }

    /// The part of the viewBox to show. The compact box keeps the bezel stroke whole.
    private var box: CGRect {
        compact ? CGRect(x: 28, y: 50, width: 144, height: 126) : CGRect(x: 0, y: 0, width: 200, height: 240)
    }

    private var scale: CGFloat { size / box.width }

    var body: some View {
        let s = scale
        ZStack(alignment: .topLeading) {
            if !compact {
                part(.neck, fill: Color(hex: 0x8C90A0), stroke: Color(hex: 0x4D5160), width: 2.5)
                part(.base, fill: Color(hex: 0x6F7384), stroke: Color(hex: 0x4D5160), width: 2.5)
                RobotShape(part: .bolts, box: box).fill(Color(hex: 0x4D5160))
                RobotShape(part: .label, box: box)
                    .fill(LinearGradient(colors: [Color(hex: 0x9DB3EE), Color(hex: 0xB98BD8)],
                                         startPoint: .topLeading, endPoint: .bottomTrailing))
                RobotShape(part: .label, box: box).stroke(Color(hex: 0x5E5F98), lineWidth: 2.5 * s)
                Text("eve")
                    .font(BrandFont.display(22 * s, black: false))
                    .foregroundStyle(Color(hex: 0xF4F4FB))
                    .lineLimit(1)
                    .frame(width: 156 * s, height: 34 * s)
                    .offset(x: 22 * s, y: 10 * s)
                part(.brow, fill: Color(hex: 0xA487D0), stroke: Color(hex: 0x5E5F98), width: 2.5)
            }
            part(.bezel, fill: Color(hex: 0xC3C6CF), stroke: Color(hex: 0x5C6070), width: 3)
            RobotShape(part: .screen, box: box).fill(Color.brandScreen)
            RobotShape(part: .eyes, box: box).fill(Color.brandEye)
            if let line, !line.isEmpty {
                Text(line)
                    .font(BrandFont.screen(22 * s))
                    .foregroundStyle(Color.brandEye)
                    .lineLimit(1)
                    .minimumScaleFactor(0.5)
                    .frame(width: 108 * s, height: 26 * s)
                    .offset(x: (46 - box.minX) * s, y: (114 - box.minY) * s)
            }
        }
        .frame(width: size, height: box.height * s, alignment: .topLeading)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(line.map { "eve the robot, its screen says \($0)" } ?? "eve the robot")
    }

    private func part(_ p: RobotShape.Part, fill: Color, stroke: Color, width: CGFloat) -> some View {
        ZStack {
            RobotShape(part: p, box: box).fill(fill)
            RobotShape(part: p, box: box).stroke(stroke, lineWidth: width * scale)
        }
    }
}

/// One piece of the robot, in viewBox coordinates, scaled to fill its frame.
private struct RobotShape: Shape {
    enum Part { case neck, base, bolts, label, brow, bezel, screen, eyes }
    let part: Part
    let box: CGRect

    func path(in rect: CGRect) -> Path {
        var p = Path()
        func rr(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, _ r: CGFloat) {
            p.addRoundedRect(in: CGRect(x: x, y: y, width: w, height: h), cornerSize: CGSize(width: r, height: r))
        }
        switch part {
        case .neck: p.addRect(CGRect(x: 68, y: 168, width: 64, height: 30))
        case .base: rr(46, 194, 108, 38, 7)
        case .bolts:
            p.addEllipse(in: CGRect(x: 59, y: 215, width: 10, height: 10))
            p.addEllipse(in: CGRect(x: 131, y: 215, width: 10, height: 10))
        case .label: rr(22, 10, 156, 34, 7)
        case .brow: rr(12, 42, 176, 14, 4)
        case .bezel: rr(30, 52, 140, 122, 14)
        case .screen: rr(42, 62, 116, 100, 6)
        case .eyes:
            // The deck's arched pixel eyes: M72 75 H88 V80 H92 V94 H87 V89 H73 V94 H68 V80 H72 Z, and the same at +40.
            for dx in [CGFloat(0), 40] {
                p.addLines([
                    CGPoint(x: 72 + dx, y: 75), CGPoint(x: 88 + dx, y: 75), CGPoint(x: 88 + dx, y: 80),
                    CGPoint(x: 92 + dx, y: 80), CGPoint(x: 92 + dx, y: 94), CGPoint(x: 87 + dx, y: 94),
                    CGPoint(x: 87 + dx, y: 89), CGPoint(x: 73 + dx, y: 89), CGPoint(x: 73 + dx, y: 94),
                    CGPoint(x: 68 + dx, y: 94), CGPoint(x: 68 + dx, y: 80), CGPoint(x: 72 + dx, y: 80),
                ])
                p.closeSubpath()
            }
        }
        let sx = rect.width / box.width, sy = rect.height / box.height
        let t = CGAffineTransform(translationX: -box.minX, y: -box.minY)
            .concatenating(CGAffineTransform(scaleX: sx, y: sy))
            .concatenating(CGAffineTransform(translationX: rect.minX, y: rect.minY))
        return p.applying(t)
    }
}

// MARK: - Buttons

/// A chunky paper button: ink border, solid ink offset shadow, presses in.
struct ChunkyButtonStyle: ButtonStyle {
    var fill: Color = .brandSheet
    var size: CGFloat = 13

    func makeBody(configuration: Configuration) -> some View {
        ChunkyButtonBody(configuration: configuration, fill: fill, size: size)
    }
}

private struct ChunkyButtonBody: View {
    let configuration: ButtonStyleConfiguration
    let fill: Color
    let size: CGFloat
    @Environment(\.isFocused) private var isFocused
    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        let pressed = configuration.isPressed
        configuration.label
            .font(BrandFont.body(size, bold: true))
            .foregroundStyle(Color.brandInk)
            .padding(.horizontal, 11)
            .padding(.vertical, 5)
            .background(RoundedRectangle(cornerRadius: 8).fill(fill))
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color.brandInk, lineWidth: 2))
            .overlay(
                RoundedRectangle(cornerRadius: 10)
                    .strokeBorder(Color.brandPink, lineWidth: 3)
                    .padding(-4)
                    .opacity(isFocused ? 1 : 0)
            )
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color.brandInk)
                    .offset(x: pressed ? 1 : 3, y: pressed ? 1 : 3)
            )
            .offset(x: pressed ? 2 : 0, y: pressed ? 2 : 0)
            .opacity(isEnabled ? 1 : 0.5)
            .contentShape(RoundedRectangle(cornerRadius: 8))
    }
}

/// A tab in a row of paper tabs: sun when selected, sheet otherwise.
struct TabPillStyle: ButtonStyle {
    let selected: Bool

    func makeBody(configuration: Configuration) -> some View {
        let pressed = configuration.isPressed
        let lift: CGFloat = selected && !pressed ? 2 : 0
        return configuration.label
            .font(BrandFont.body(12, bold: true))
            .foregroundStyle(Color.brandInk)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: true)
            .padding(.horizontal, 11)
            .padding(.vertical, 4)
            .background(Capsule().fill(selected ? Color.brandSun : Color.brandSheet))
            .overlay(Capsule().strokeBorder(Color.brandInk, lineWidth: 2))
            .background(Capsule().fill(Color.brandInk).offset(x: lift, y: lift).opacity(lift > 0 ? 1 : 0))
            .offset(x: pressed ? 1 : 0, y: pressed ? 1 : 0)
            .contentShape(Capsule())
    }
}
