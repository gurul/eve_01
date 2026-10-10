// The composer's / menu (docs/app-chat.md, "The composer"): Telegram's command menu, in the app's chat. While the field
// starts with "/" and has no space, a small card above the composer lists eve's code words that start with what was
// typed; a tap sends that word at once, as Telegram's menu does. Which words, and the matching: Shared/SlashCommands.swift.
import SwiftUI

/// The rows above the composer card: "/command" and its line, at most `visibleRows` tall, then it scrolls.
struct SlashMenuList: View {
    var rows: [SlashCommand]
    var pick: (SlashCommand) -> Void

    static let rowHeight: CGFloat = 44
    static let visibleRows = 6

    /// The card's height for `count` rows: up to six, and half a seventh past that to say "more below".
    static func height(_ count: Int) -> CGFloat {
        CGFloat(min(count, visibleRows)) * rowHeight + 12 + (count > visibleRows ? rowHeight / 2 : 0)
    }

    var body: some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                ForEach(rows) { row in
                    Button { pick(row) } label: {
                        HStack(spacing: 10) {
                            Text(row.text)
                                .font(.body.weight(.medium))
                                .foregroundStyle(.primary)
                                .lineLimit(1)
                                .layoutPriority(1)
                            Text(row.description)
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.tail)
                            Spacer(minLength: 0)
                        }
                        .padding(.horizontal, 16)
                        .frame(height: Self.rowHeight)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(SlashRowStyle())
                    .accessibilityLabel(row.command.replacingOccurrences(of: "_", with: " ") + " command")
                    .accessibilityValue(row.description)
                    .accessibilityHint("Sends \(row.text) to eve")
                }
            }
            .padding(.vertical, 6)
        }
        .scrollBounceBehavior(.basedOnSize)
        .frame(height: Self.height(rows.count))
        .background(RoundedRectangle(cornerRadius: 20, style: .continuous).fill(Color.eveFill))
        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(Color.eveEdge, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Commands")
    }
}

/// A row: no chrome, the selected fill while pressed.
private struct SlashRowStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(configuration.isPressed ? Color.eveSelected : Color.clear)
    }
}
