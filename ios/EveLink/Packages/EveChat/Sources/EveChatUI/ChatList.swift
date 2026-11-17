// The chat's message list, native (docs/app-chat.md): the whole conversation oldest first, pinned to its end as
// replies stream, padded by what the app's top bar and composer cover, older pages loaded as the owner scrolls up,
// and Horizontal History's cards above the composer.
import EveChatCore
import SwiftUI
#if canImport(UIKit)
import UIKit
#endif

public struct ChatList: View {
    private let engine: ChatEngine
    private let top: CGFloat
    private let bottom: CGFloat

    /// `top` and `bottom`: how much of the list the app's top bar and composer (and the keyboard) cover.
    public init(engine: ChatEngine, top: CGFloat = 0, bottom: CGFloat = 0) {
        self.engine = engine
        self.top = top
        self.bottom = bottom
    }

    @State private var position = ScrollPosition(edge: .bottom)
    @State private var atBottom = true
    /// The list follows its end: true until the owner scrolls away, and again once they come back to it.
    @State private var pinned = true
    @State private var dragging = false
    @State private var nearTop = false
    @State private var seenOlder = 0
    @State private var glow: String?
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// Within this many points of the end counts as "at the bottom".
    static let pinSlack: CGFloat = 24
    /// Closer than this to the first loaded message, the page before it is fetched.
    static let olderWithin: CGFloat = 900
    private static let end = "chat-end"

    public var body: some View {
        ScrollView {
            LazyVStack(spacing: ChatTheme.gap) {
                ForEach(engine.messages) { message in
                    row(message)
                        .background(alignment: .center) { if glow == message.id { Glow() } }
                        .modifier(Column())
                        .id(message.id)
                }
                ForEach(engine.queued) { item in
                    OwnerBubble(text: item.text, attachments: item.attachments, queued: true)
                        .modifier(Column())
                        .id(item.id)
                        .transition(reduceMotion ? .opacity : .opacity.combined(with: .offset(y: 8)))
                }
                if engine.showsTyping { TypingDots().modifier(Column()).id("chat-typing") }
                Color.clear.frame(height: 1).id(Self.end)
            }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.22), value: engine.queued)
        }
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom)
        // A list at its end stays there when its content or its room changes (a reply growing, a card laid out
        // late, the keyboard), done by the scroll view itself. Nothing here scrolls in answer to a layout change:
        // a lazily measured row can change the content's height on every pass, and a scroll per change never ends.
        .defaultScrollAnchor(pinned ? .bottom : nil, for: .sizeChanges)
        .contentMargins(.top, top + 8, for: .scrollContent)
        .contentMargins(.bottom, bottom + 16, for: .scrollContent)
        .contentMargins(.top, top, for: .scrollIndicators)
        .contentMargins(.bottom, bottom, for: .scrollIndicators)
        .scrollDismissesKeyboard(.interactively)
        // A tap on the conversation puts the keyboard away, as a drag down does.
        .simultaneousGesture(TapGesture().onEnded { ChatKeyboard.dismiss() })
        .onScrollGeometryChange(for: Bool.self) { g in
            g.contentSize.height - g.contentOffset.y - g.containerSize.height + g.contentInsets.bottom <= Self.pinSlack
        } action: { _, now in
            atBottom = now
            if now && !dragging { pinned = true }
        }
        .onScrollPhaseChange { _, now in
            switch now {
            case .interacting:
                dragging = true
                pinned = false
            case .idle:
                if dragging { pinned = atBottom }
                dragging = false
            default:
                break
            }
        }
        .onScrollGeometryChange(for: Bool.self) { g in
            g.contentOffset.y + g.contentInsets.top < Self.olderWithin
        } action: { _, now in
            nearTop = now
            if now { engine.loadOlder() }
        }
        // The conversation arrived: open on its end, with no travel.
        .onChange(of: engine.historyLoaded) {
            pinned = true
            toEnd(animated: false)
        }
        // The owner sent something: show it, wherever the list was.
        .onChange(of: engine.sendTick) {
            pinned = true
            toEnd(animated: true)
        }
        // A line came on its own, a reply grew, or the dots came up: follow only from the end.
        .onChange(of: engine.arrivalTick) { if pinned { toEnd(animated: true) } }
        .onChange(of: engine.showsTyping) { if pinned { toEnd(animated: true) } }
        // The keyboard or the composer moved: a list at its end stays there.
        .onChange(of: bottom) { if pinned { toEnd(animated: false) } }
        // An older page went in above: stay on the message that was first.
        .onChange(of: engine.messages.first?.id) { was, _ in
            guard engine.olderTick != seenOlder else { return }
            seenOlder = engine.olderTick
            // A list at its end stays there (a short conversation filling the screen); one the owner scrolled up
            // stays on the message they were reading.
            if pinned { toEnd(animated: false) } else if let was { position.scrollTo(id: was, anchor: .top) }
            if nearTop { engine.loadOlder() }
        }
        .onChange(of: engine.jump) { _, jump in
            guard let jump else { return }
            pinned = false
            withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.35)) { position.scrollTo(id: jump.id, anchor: .center) }
            glow = jump.id
            Task {
                try? await Task.sleep(for: .seconds(1.6))
                if glow == jump.id { glow = nil }
            }
        }
        .overlay(alignment: .bottomTrailing) {
            if !pinned && !atBottom && !engine.historyShown {
                Button {
                    pinned = true
                    toEnd(animated: true)
                } label: {
                    Image(systemName: "arrow.down")
                        .font(.system(size: 15, weight: .semibold))
                        .frame(width: 44, height: 44)
                        .background(theme.surface(scheme), in: Circle())
                        .overlay(Circle().strokeBorder(theme.line(scheme), lineWidth: 1))
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Scroll to latest")
                .padding(.trailing, ChatTheme.side)
                .padding(.bottom, bottom + 12)
                .transition(.opacity)
            }
        }
        .overlay(alignment: .bottom) {
            HistoryRow(engine: engine).padding(.bottom, bottom)
        }
        .animation(reduceMotion ? nil : .easeOut(duration: 0.15), value: pinned || atBottom)
    }

    @ViewBuilder
    private func row(_ message: ChatMessage) -> some View {
        switch message.role {
        case .owner:
            OwnerBubble(text: message.text, attachments: message.attachments, reaction: engine.reaction(for: message.id),
                        problem: engine.problems[message.id])
        case .eve:
            EveMessage(message: message) { human, data in engine.ask(human, data: data) }
        }
    }

    private func toEnd(animated: Bool) {
        if animated && !reduceMotion {
            withAnimation(.easeOut(duration: 0.25)) { position.scrollTo(edge: .bottom) }
        } else {
            position.scrollTo(edge: .bottom)
        }
    }
}

/// The keyboard, put away from anywhere.
@MainActor
enum ChatKeyboard {
    static func dismiss() {
        #if canImport(UIKit)
        UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
        #endif
    }
}

/// Every row spans the list, so a row scrolled to by id never moves the list sideways; its content keeps the side
/// padding and, wider than a phone, a readable column.
private struct Column: ViewModifier {
    func body(content: Content) -> some View {
        content
            .frame(maxWidth: ChatTheme.column)
            .padding(.horizontal, ChatTheme.side)
            .frame(maxWidth: .infinity)
    }
}

/// The soft light behind a message Horizontal History jumped to.
private struct Glow: View {
    @State private var on = true
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        RoundedRectangle(cornerRadius: 16, style: .continuous)
            .fill(Color.accentColor.opacity(on ? 0.16 : 0))
            .padding(-8)
            .onAppear { withAnimation(reduceMotion ? nil : .easeOut(duration: 1.6)) { on = false } }
            .allowsHitTesting(false)
    }
}
