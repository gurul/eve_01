// A picture list with gaps (docs/app-chat.md, "The card engine"): when a List asks for pictures and at least half
// its items have one, an item with none gets a soft two-colour gradient, the same one every time for the same
// title. The rules and the hues are those of the web page (jsonRegistry.tsx, showsPictures and gradientFor).
import SwiftUI

/// Pictures are shown when the block asks for them and at least half the items have one.
func showsPictures(variant: String, sources: [String]) -> Bool {
    let withPicture = sources.filter { !$0.isEmpty }.count
    return variant == "image" && withPicture > 0 && withPicture * 2 >= sources.count
}

/// The first hue of a title's gradient, 0 to 359. The second is 50 further round the wheel.
func gradientHue(_ title: String) -> Int {
    var hue = 0
    for scalar in title.unicodeScalars { hue = (hue * 31 + Int(scalar.value)) % 360 }
    return hue
}

/// The gradient for a title: hsl(h 70% 62%) at the top left to hsl(h+50 70% 46%) at the bottom right.
func titleGradient(_ title: String) -> LinearGradient {
    let hue = gradientHue(title)
    return LinearGradient(
        colors: [hsl(hue, 0.70, 0.62), hsl((hue + 50) % 360, 0.70, 0.46)],
        startPoint: .topLeading,
        endPoint: .bottomTrailing
    )
}

/// A CSS hsl() colour.
func hsl(_ hue: Int, _ saturation: Double, _ lightness: Double) -> Color {
    let (r, g, b) = hslComponents(hue, saturation, lightness)
    return Color(red: r, green: g, blue: b)
}

func hslComponents(_ hue: Int, _ saturation: Double, _ lightness: Double) -> (Double, Double, Double) {
    let chroma = (1 - abs(2 * lightness - 1)) * saturation
    let sector = Double(hue % 360) / 60
    let x = chroma * (1 - abs(sector.truncatingRemainder(dividingBy: 2) - 1))
    let (r, g, b): (Double, Double, Double)
    switch sector {
    case ..<1: (r, g, b) = (chroma, x, 0)
    case ..<2: (r, g, b) = (x, chroma, 0)
    case ..<3: (r, g, b) = (0, chroma, x)
    case ..<4: (r, g, b) = (0, x, chroma)
    case ..<5: (r, g, b) = (x, 0, chroma)
    default: (r, g, b) = (chroma, 0, x)
    }
    let m = lightness - chroma / 2
    return (r + m, g + m, b + m)
}
