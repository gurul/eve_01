// Horizontal History (docs/app-chat.md): topic cards that slide up above the composer, swiped sideways. Saved ones
// first. A tap jumps to the topic, or picks it back up when it is no longer loaded; the star saves it.
import EveChatCore
import SwiftUI

struct HistoryRow: View {
    let engine: ChatEngine
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let cards = engine.topics.cards
        VStack(alignment: .leading, spacing: 0) {
            if cards.isEmpty {
                Text("No past topics yet.").font(.caption).foregroundStyle(theme.dim(scheme))
                    .padding(.horizontal, ChatTheme.side).padding(.vertical, 6)
            }
            ScrollView(.horizontal) {
                LazyHStack(alignment: .top, spacing: 10) {
                    ForEach(cards) { topic in
                        TopicCard(topic: topic, open: { engine.open(topic) }, star: { engine.star(topic) })
                            .containerRelativeFrame(.horizontal) { width, _ in min(width * 0.72, 300) }
                    }
                }
                .scrollTargetLayout()
                .padding(.horizontal, ChatTheme.side)
                .padding(.top, 8)
                .padding(.bottom, 10)
            }
            .scrollTargetBehavior(.viewAligned)
            .scrollIndicators(.hidden)
            .frame(height: cards.isEmpty ? 0 : 114)
            if !engine.historyNote.isEmpty {
                Text(engine.historyNote).font(.caption).foregroundStyle(theme.dim(scheme))
                    .padding(.horizontal, ChatTheme.side).padding(.bottom, 6)
                    .accessibilityAddTraits(.updatesFrequently)
            }
        }
        .padding(.top, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(alignment: .top) {
            // The band, and a short fade above it so the chat runs into it softly.
            VStack(spacing: 0) {
                LinearGradient(colors: [theme.background(scheme).opacity(0), theme.background(scheme)],
                               startPoint: .top, endPoint: .bottom).frame(height: 14)
                theme.background(scheme)
            }
            .padding(.top, -14)
        }
        .opacity(engine.historyShown ? 1 : 0)
        .offset(y: engine.historyShown ? 0 : 24)
        .allowsHitTesting(engine.historyShown)
        .accessibilityHidden(!engine.historyShown)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Conversation history")
        .animation(reduceMotion ? nil : .easeOut(duration: 0.22), value: engine.historyShown)
    }
}

private struct TopicCard: View {
    let topic: ChatTopic
    let open: () -> Void
    let star: () -> Void
    @Environment(\.chatTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var meta: String {
        "\(topic.count) \(topic.count == 1 ? "message" : "messages") · \(ChatTopics.when(topic.endedAt))"
    }

    var body: some View {
        Button(action: open) {
            VStack(alignment: .leading, spacing: 4) {
                Text(topic.title).font(.system(size: 15, weight: .semibold)).lineLimit(2)
                if !topic.preview.isEmpty {
                    Text(topic.preview).font(.system(size: 13)).foregroundStyle(theme.dim(scheme)).lineLimit(2)
                }
                Spacer(minLength: 0)
                Text(meta).font(.system(size: 11)).monospacedDigit().foregroundStyle(theme.dim(scheme))
            }
            .multilineTextAlignment(.leading)
            .padding(EdgeInsets(top: 12, leading: 14, bottom: 11, trailing: 44))
            .frame(maxWidth: .infinity, minHeight: 92, alignment: .topLeading)
            .background(theme.surface(scheme), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(theme.line(scheme), lineWidth: 0.5))
            .contentShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(topic.title), \(meta)")
        .overlay(alignment: .topTrailing) {
            Button(action: star) {
                Image(systemName: topic.bookmarked ? "star.fill" : "star")
                    .font(.system(size: 16))
                    .foregroundStyle(topic.bookmarked ? ChatTheme.star : theme.dim(scheme))
                    .frame(width: 44, height: 44)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(topic.bookmarked ? "Remove from saved" : "Save topic")
            .accessibilityAddTraits(topic.bookmarked ? .isSelected : [])
            .padding(2)
        }
    }
}
