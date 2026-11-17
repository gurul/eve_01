// One message: the owner's bubble (attachments above it, eve's reaction on its corner), or eve's reply (markdown,
// cards, pictures and files, and the memory lines it leaned on).
import EveCardUI
import EveChatCore
import EveMarkdown
import JSONRenderCore
import SwiftUI

struct OwnerBubble: View {
    let text: String
    let attachments: [ChatAttachment]
    var reaction = ""
    var problem: String?
    var queued = false
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            AttachmentList(items: attachments, side: .owner)
                .opacity(queued ? 0.5 : 1)
            if !text.isEmpty {
                Text(text)
                    .font(.body)
                    .foregroundStyle(theme.ownerText(scheme))
                    .textSelection(.enabled)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 9)
                    .background(theme.ownerBubble(scheme), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                    .opacity(queued ? 0.5 : 1)
                    .overlay(alignment: .bottomLeading) {
                        if !reaction.isEmpty { ReactionBadge(emoji: reaction).offset(x: -13, y: 15) }
                    }
                    .padding(.bottom, reaction.isEmpty ? 0 : 12)
            } else if !reaction.isEmpty {
                ReactionBadge(emoji: reaction)
            }
            if queued {
                Text("Queued").font(.caption).foregroundStyle(theme.dim(scheme)).padding(.trailing, 6).padding(.top, -2)
            }
            if let problem {
                Text(problem).font(.caption).foregroundStyle(.red).multilineTextAlignment(.trailing)
                    .accessibilityLabel("Not sent: \(problem)")
            }
        }
        .containerRelativeFrame(.horizontal, alignment: .trailing) { width, _ in (width - 2 * ChatTheme.side) * ChatTheme.ownerWidth }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .accessibilityElement(children: .combine)
    }
}

/// eve's reaction: a small badge ringed in the screen's own colour, so it reads as sitting on the bubble.
struct ReactionBadge: View {
    let emoji: String
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var shown = false

    var body: some View {
        Text(emoji)
            .font(.system(size: 15))
            .padding(.horizontal, 5)
            .frame(minWidth: 26, minHeight: 26)
            .background(scheme == .dark ? Color(white: 0.23) : Color(white: 0.915), in: Capsule())
            .overlay(Capsule().strokeBorder(theme.background(scheme), lineWidth: 2).padding(-2))
            .scaleEffect(shown || reduceMotion ? 1 : 0.4)
            .opacity(shown || reduceMotion ? 1 : 0)
            .onAppear { withAnimation(.spring(response: 0.26, dampingFraction: 0.6)) { shown = true } }
            .id(emoji)
            .accessibilityLabel("eve reacted \(emoji)")
            .allowsHitTesting(false)
    }
}

struct AttachmentList: View {
    enum Side { case owner, eve }
    let items: [ChatAttachment]
    let side: Side
    @Environment(\.chatActions) private var actions
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        if !items.isEmpty {
            let solo = items.count == 1
            WrapRow(spacing: 6, trailing: side == .owner) {
                ForEach(items) { a in
                    Button { if !a.gone { actions.openAttachment(a) } } label: { tile(a, solo: solo) }
                        .buttonStyle(.plain)
                        .disabled(a.gone)
                        .accessibilityLabel(a.gone ? "\(Self.word(a.kind)): \(a.name), expired" : "Open \(Self.word(a.kind)): \(a.name)")
                }
            }
        }
    }

    @ViewBuilder
    private func tile(_ a: ChatAttachment, solo: Bool) -> some View {
        if let image = Thumbs.image(a) {
            let ratio = CGFloat(image.width) / CGFloat(max(1, image.height))
            let limit: CGFloat = side == .eve ? 240 : 200
            if solo {
                Image(decorative: image, scale: 1)
                    .resizable()
                    .aspectRatio(ratio, contentMode: .fit)
                    .frame(maxWidth: limit, maxHeight: 260)
                    .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
            } else {
                Image(decorative: image, scale: 1)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
                    .frame(width: 96, height: 96)
                    .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            }
        } else {
            HStack(spacing: 6) {
                Image(systemName: a.kind == .image ? "photo" : "doc")
                    .font(.system(size: 14, weight: .medium))
                Text(a.name).font(.subheadline).lineLimit(1).truncationMode(.middle)
            }
            .foregroundStyle(a.gone ? theme.dim(scheme) : .primary)
            .padding(.horizontal, 11)
            .frame(minHeight: 44)
            .frame(maxWidth: 240)
            .fixedSize(horizontal: true, vertical: false)
            .background(theme.surface(scheme), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
    }

    static func word(_ kind: ChatAttachment.Kind) -> String {
        switch kind {
        case .image: "Photo"
        case .pdf: "PDF"
        case .text: "Text"
        case .doc: "Document"
        case .file: "File"
        }
    }
}

/// Children left to right, wrapping, the owner's rows packed to the trailing edge.
struct WrapRow: Layout {
    var spacing: CGFloat = 6
    var trailing = false

    private func rows(_ subviews: Subviews, width: CGFloat) -> [(items: [(Int, CGSize)], width: CGFloat, height: CGFloat)] {
        var out: [(items: [(Int, CGSize)], width: CGFloat, height: CGFloat)] = [([], 0, 0)]
        for (i, view) in subviews.enumerated() {
            let size = view.sizeThatFits(ProposedViewSize(width: width, height: nil))
            var row = out[out.count - 1]
            if !row.items.isEmpty, row.width + spacing + size.width > width {
                out.append(([], 0, 0))
                row = out[out.count - 1]
            }
            row.width += (row.items.isEmpty ? 0 : spacing) + size.width
            row.height = max(row.height, size.height)
            row.items.append((i, size))
            out[out.count - 1] = row
        }
        return out.filter { !$0.items.isEmpty }
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let all = rows(subviews, width: proposal.width ?? .infinity)
        let height = all.map(\.height).reduce(0, +) + spacing * CGFloat(max(0, all.count - 1))
        return CGSize(width: all.map(\.width).max() ?? 0, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in rows(subviews, width: bounds.width) {
            var x = trailing ? bounds.maxX - row.width : bounds.minX
            for (i, size) in row.items {
                subviews[i].place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + spacing
        }
    }
}

/// eve's reply: prose as markdown, each card drawn natively, in the order eve wrote them.
struct EveMessage: View {
    let message: ChatMessage
    /// A card's tap that continues the conversation; returns a line when it did not go.
    let ask: (String, [Any]) -> String
    @Environment(\.chatActions) private var actions
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let parts = ReplyParts.split(message.text, streaming: message.streaming)
        VStack(alignment: .leading, spacing: 10) {
            AttachmentList(items: message.attachments, side: .eve)
            ForEach(Array(parts.enumerated()), id: \.offset) { index, part in
                switch part {
                case .markdown(let text):
                    // Only the part still being written is read as unfinished markdown.
                    MarkdownView(compactLinks(text), streaming: message.streaming && index == parts.count - 1)
                case .card(let code, let complete):
                    CardHost(stateKey: "\(message.id)#\(index)", code: code, streaming: message.streaming && !complete, ask: ask)
                case .legacyCard:
                    Text("This card was made in an older format. Ask eve again to see it.")
                        .font(.footnote)
                        .foregroundStyle(theme.dim(scheme))
                }
            }
            SourceList(messageID: message.id, items: message.sources)
        }
        .environment(\.openURL, OpenURLAction { url in
            actions.openURL(url)
            return .handled
        })
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// One card, with the one line a tap can earn ("Wait for Eve to finish…").
struct CardHost: View {
    let stateKey: String
    let code: String
    let streaming: Bool
    let ask: (String, [Any]) -> String
    @Environment(\.chatActions) private var actions
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme
    @State private var note = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            JsonCardView(stateKey: stateKey, code: code, streaming: streaming,
                         onAsk: { human, data in
                             note = ask(human, CardHost.plain(data))
                             if !note.isEmpty { clearLater(note) }
                         },
                         onOpen: { if let url = URL(string: $0) { actions.openURL(url) } })
                .cardMarkdown { AnyView(MarkdownView(compactLinks($0))) }
            if !note.isEmpty {
                Text(note).font(.footnote).foregroundStyle(theme.dim(scheme)).accessibilityAddTraits(.updatesFrequently)
            }
        }
    }

    /// A tap's form values as plain JSON, for the owner's message: none, or the one value the card sent.
    static func plain(_ data: JSONValue?) -> [Any] {
        guard let data, let bytes = try? JSONEncoder().encode(data),
              let value = try? JSONSerialization.jsonObject(with: bytes, options: .fragmentsAllowed) else { return [] }
        return [value]
    }

    private func clearLater(_ shown: String) {
        Task {
            try? await Task.sleep(for: .seconds(4))
            if note == shown { note = "" }
        }
    }
}

/// The memory lines a reply leaned on: one small muted line at rest; open, each on its own line.
struct SourceList: View {
    let messageID: String
    let items: [ChatSource]
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var expanded = false

    /// Which lists are open, so one stays open when its row scrolls away and back.
    @MainActor private static var open = Set<String>()

    var body: some View {
        if !items.isEmpty {
            VStack(alignment: .leading, spacing: 2) {
                Button {
                    withAnimation(reduceMotion ? nil : .easeOut(duration: 0.16)) { expanded.toggle() }
                    if expanded { Self.open.insert(messageID) } else { Self.open.remove(messageID) }
                } label: {
                    HStack(spacing: 4) {
                        Text("Sources · \(items.count)")
                        Image(systemName: "chevron.right")
                            .font(.system(size: 9, weight: .semibold))
                            .rotationEffect(.degrees(expanded ? 90 : 0))
                    }
                    .frame(minHeight: 28)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Sources, \(items.count)")
                .accessibilityValue(expanded ? "expanded" : "collapsed")
                if expanded {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(items.enumerated()), id: \.offset) { _, s in
                            VStack(alignment: .leading, spacing: 1) {
                                HStack(alignment: .firstTextBaseline, spacing: 6) {
                                    Text(s.kind.label.uppercased()).fontWeight(.semibold).kerning(0.2)
                                    if let when = s.when { Text(when) }
                                }
                                .font(.system(size: 11))
                                Text(s.text).foregroundStyle(.primary.opacity(0.85)).textSelection(.enabled)
                            }
                        }
                    }
                    .padding(.leading, 10)
                    .padding(.vertical, 2)
                    .overlay(alignment: .leading) { Rectangle().fill(Color.gray.opacity(0.22)).frame(width: 2) }
                }
            }
            .font(.system(size: 13))
            .foregroundStyle(theme.dim(scheme))
            .onAppear { expanded = Self.open.contains(messageID) }
        }
    }
}

/// Three dots: eve is replying and no words are on the screen yet.
struct TypingDots: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 30, paused: reduceMotion)) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            HStack(spacing: 5) {
                ForEach(0..<3, id: \.self) { i in
                    let phase = reduceMotion ? 0.5 : (sin((t * 1.4 - Double(i) * 0.18) * 2 * .pi) + 1) / 2
                    Circle().fill(theme.dim(scheme)).frame(width: 7, height: 7).opacity(0.35 + 0.65 * phase)
                }
            }
        }
        .frame(height: 24)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement()
        .accessibilityLabel("eve is replying")
    }
}
