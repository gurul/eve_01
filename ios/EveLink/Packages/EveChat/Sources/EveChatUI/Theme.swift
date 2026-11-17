// The chat's colours and sizes, and what the views ask the app to do. SwiftUI only: the app owns haptics, Quick Look
// and Safari, and passes them in as `ChatActions`.
import EveChatCore
import ImageIO
import SwiftUI

public struct ChatTheme: Sendable {
    public var ownerBubble: @Sendable (ColorScheme) -> Color
    public var ownerText: @Sendable (ColorScheme) -> Color
    /// A raised surface: a topic card, a file chip, the reaction badge.
    public var surface: @Sendable (ColorScheme) -> Color
    /// The screen behind the list: the reaction badge is ringed in it.
    public var background: @Sendable (ColorScheme) -> Color
    public var dim: @Sendable (ColorScheme) -> Color
    public var line: @Sendable (ColorScheme) -> Color

    public init(ownerBubble: @escaping @Sendable (ColorScheme) -> Color = { $0 == .dark ? Color(white: 0.17) : Color(white: 0.91) },
                ownerText: @escaping @Sendable (ColorScheme) -> Color = { $0 == .dark ? .white : Color(white: 0.11) },
                surface: @escaping @Sendable (ColorScheme) -> Color = { $0 == .dark ? Color(white: 0.11) : Color(red: 0.95, green: 0.95, blue: 0.97) },
                background: @escaping @Sendable (ColorScheme) -> Color = { $0 == .dark ? .black : .white },
                dim: @escaping @Sendable (ColorScheme) -> Color = { $0 == .dark ? Color(red: 0.92, green: 0.92, blue: 0.96).opacity(0.6) : Color(red: 0.24, green: 0.24, blue: 0.26).opacity(0.6) },
                line: @escaping @Sendable (ColorScheme) -> Color = { $0 == .dark ? Color.white.opacity(0.12) : Color(red: 0.24, green: 0.24, blue: 0.26).opacity(0.12) }) {
        self.ownerBubble = ownerBubble
        self.ownerText = ownerText
        self.surface = surface
        self.background = background
        self.dim = dim
        self.line = line
    }

    static let star = Color(red: 1, green: 0.62, blue: 0.04)
    /// The list's side padding, the gap between messages, and how wide an owner bubble may be.
    static let side: CGFloat = 16
    static let gap: CGFloat = 14
    static let ownerWidth: CGFloat = 0.78
    /// Wider than a phone, the list keeps a readable column.
    static let column: CGFloat = 728
}

/// What a tap in the chat asks the app for.
public struct ChatActions {
    /// A link in a reply or a card.
    public var openURL: (URL) -> Void
    /// A photo or file, to show full size.
    public var openAttachment: (ChatAttachment) -> Void

    public init(openURL: @escaping (URL) -> Void = { _ in }, openAttachment: @escaping (ChatAttachment) -> Void = { _ in }) {
        self.openURL = openURL
        self.openAttachment = openAttachment
    }
}

extension EnvironmentValues {
    @Entry var chatTheme = ChatTheme()
    @Entry var chatActions = ChatActions()
}

/// A thumbnail's pixels, decoded once per attachment.
@MainActor
enum Thumbs {
    private static var cache: [String: CGImage] = [:]

    static func image(_ a: ChatAttachment) -> CGImage? {
        guard let thumb = a.thumb else { return nil }
        let key = "\(a.id):\(thumb.utf8.count)"
        if let hit = cache[key] { return hit }
        guard let data = a.thumbData, let source = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else { return nil }
        if cache.count > 200 { cache.removeAll() }
        cache[key] = image
        return image
    }
}

public extension View {
    /// The chat's colours, from the app's own palette.
    func chatTheme(_ theme: ChatTheme) -> some View { environment(\.chatTheme, theme) }
    /// What a tapped link or attachment does.
    func chatActions(_ actions: ChatActions) -> some View { environment(\.chatActions, actions) }
}
