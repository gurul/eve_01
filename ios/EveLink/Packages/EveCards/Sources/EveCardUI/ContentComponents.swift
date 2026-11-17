// The components that show something: Text, Callout, Table, Facts, List, Steps, Image, Tags and FollowUps.
// Behaviour: docs/app-chat.md, "The card engine".
import SwiftUI
import JSONRenderCore

/// A short paragraph. Markdown is drawn by the card's host.
struct TextBlock: View {
    let text: String

    var body: some View {
        if !text.isEmpty {
            CardMarkdown(text: text)
                .font(.body)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A banner for something the owner must not miss.
struct CalloutBlock: View {
    let variant: String
    let title: String
    let description: String

    private var look: (symbol: String, tint: Color?, spoken: String) {
        switch variant {
        case "warning": return ("exclamationmark.triangle.fill", .orange, "Warning")
        case "success": return ("checkmark.circle.fill", .green, "Success")
        case "neutral": return ("info.circle.fill", nil, "Note")
        default: return ("info.circle.fill", .blue, "Note")
        }
    }

    var body: some View {
        let look = look
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: look.symbol)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(look.tint.map(AnyShapeStyle.init) ?? AnyShapeStyle(.secondary))
                .accessibilityLabel(look.spoken)
            VStack(alignment: .leading, spacing: 2) {
                if !title.isEmpty {
                    Text(title).font(.subheadline.weight(.semibold)).fixedSize(horizontal: false, vertical: true)
                }
                if !description.isEmpty {
                    CardMarkdown(text: description).font(.subheadline).foregroundStyle(.secondary)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(
            look.tint.map { AnyShapeStyle($0.opacity(0.14)) } ?? AnyShapeStyle(.cardFill),
            in: RoundedRectangle(cornerRadius: CardMetrics.innerCorner, style: .continuous)
        )
        .accessibilityElement(children: .combine)
    }
}

/// A table with a header row. A table wider than the card scrolls sideways inside it.
struct TableBlock: View {
    let element: CardElement

    var body: some View {
        let columns = element.texts("columns")
        let rows = element.list("rows").map { row in (row.arrayValue ?? []).map(CardElement.text) }
        if !columns.isEmpty {
            ViewThatFits(in: .horizontal) {
                grid(columns, rows, scrolls: false)
                ScrollView(.horizontal) { grid(columns, rows, scrolls: true) }
                    .scrollIndicators(.hidden)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func grid(_ columns: [String], _ rows: [[String]], scrolls: Bool) -> some View {
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 16, verticalSpacing: 0) {
            GridRow {
                ForEach(columns.indices, id: \.self) { c in
                    cell(columns[c], scrolls: scrolls)
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .padding(.bottom, 8)
                        .accessibilityAddTraits(.isHeader)
                }
            }
            ForEach(rows.indices, id: \.self) { r in
                CardRule().gridCellUnsizedAxes(.horizontal)
                GridRow {
                    ForEach(columns.indices, id: \.self) { c in
                        cell(c < rows[r].count ? rows[r][c] : "", scrolls: scrolls)
                            .font(.subheadline)
                            .monospacedDigit()
                            .padding(.vertical, 10)
                            .accessibilityLabel("\(columns[c]): \(c < rows[r].count ? rows[r][c] : "")")
                    }
                }
            }
        }
    }

    /// A cell. In a table that scrolls, a long cell wraps at a set width rather than stretching its column.
    @ViewBuilder
    private func cell(_ text: String, scrolls: Bool) -> some View {
        if scrolls {
            Text(text).lineLimit(3).frame(maxWidth: 200, alignment: .leading).fixedSize(horizontal: false, vertical: true)
        } else {
            Text(text).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Label and value pairs, one per line.
struct FactsBlock: View {
    let element: CardElement

    var body: some View {
        let rows = element.list("rows").compactMap(\.objectValue)
        RuledRows(count: rows.count) { i in
            HStack(alignment: .firstTextBaseline, spacing: 16) {
                Text(CardElement.text(rows[i]["label"])).foregroundStyle(.secondary)
                Spacer(minLength: 0)
                Text(CardElement.text(rows[i]["value"])).multilineTextAlignment(.trailing)
            }
            .font(.subheadline)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.vertical, 10)
            .accessibilityElement(children: .combine)
        }
    }
}

/// A list of options or results: numbered, or with a picture each. An item with a link opens it when tapped.
struct ListBlock: View {
    let element: CardElement
    @Environment(\.cardHost) private var host
    @ScaledMetric(relativeTo: .body) private var thumb: CGFloat = 52

    var body: some View {
        let items = element.list("items").compactMap(\.objectValue)
        let pictures = showsPictures(variant: element.text("variant"), sources: items.map { CardElement.text($0["image"]?["src"]) })
        RuledRows(count: items.count) { i in
            let item = items[i]
            let title = CardElement.text(item["title"])
            let subtitle = CardElement.text(item["subtitle"])
            let url = CardElement.text(item["url"])
            let row = HStack(alignment: pictures ? .center : .firstTextBaseline, spacing: 12) {
                if pictures {
                    picture(source: CardElement.text(item["image"]?["src"]), alt: CardElement.text(item["image"]?["alt"]), title: title)
                } else {
                    Text("\(i + 1)")
                        .font(.subheadline.weight(.medium).monospacedDigit())
                        .foregroundStyle(.secondary)
                        .frame(minWidth: 18, alignment: .leading)
                        .accessibilityHidden(true)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.body)
                    if !subtitle.isEmpty {
                        Text(subtitle).font(.subheadline).foregroundStyle(.secondary)
                    }
                }
                .multilineTextAlignment(.leading)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                if !url.isEmpty {
                    Image(systemName: "arrow.up.right")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(.tertiary)
                        .accessibilityHidden(true)
                }
            }
            .padding(.vertical, 10)
            .frame(minHeight: CardMetrics.tap)
            if url.isEmpty {
                row.accessibilityElement(children: .combine)
            } else {
                Button { host.open(url) } label: { row }
                    .buttonStyle(RowPressStyle())
                    .accessibilityElement(children: .combine)
                    .accessibilityAddTraits(.isLink)
                    .accessibilityHint("Opens the link")
            }
        }
    }

    /// An item's picture, or a soft gradient in its place when it has none.
    private func picture(source: String, alt: String, title: String) -> some View {
        let shape = RoundedRectangle(cornerRadius: 10, style: .continuous)
        return Group {
            if let url = secureURL(source) {
                AsyncImage(url: url) { phase in
                    if let image = phase.image {
                        image.resizable().scaledToFill()
                    } else {
                        Rectangle().fill(.cardFill)
                    }
                }
                .accessibilityLabel(alt)
            } else {
                titleGradient(title).accessibilityHidden(true)
            }
        }
        .frame(width: thumb, height: thumb)
        .clipShape(shape)
        .overlay(shape.strokeBorder(.cardEdge, lineWidth: 1))
    }
}

/// A step-by-step guide, numbered in order.
struct StepsBlock: View {
    let element: CardElement
    @ScaledMetric(relativeTo: .footnote) private var badge: CGFloat = 24

    var body: some View {
        let items = element.list("items").compactMap(\.objectValue)
        VStack(alignment: .leading, spacing: 14) {
            ForEach(items.indices, id: \.self) { i in
                let details = CardElement.text(items[i]["details"])
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Text("\(i + 1)")
                        .font(.footnote.weight(.semibold).monospacedDigit())
                        .frame(minWidth: badge, minHeight: badge)
                        .background(.cardFill, in: Circle())
                        .accessibilityLabel("Step \(i + 1)")
                    VStack(alignment: .leading, spacing: 2) {
                        Text(CardElement.text(items[i]["title"]))
                            .font(.body.weight(.medium))
                            .fixedSize(horizontal: false, vertical: true)
                        if !details.isEmpty {
                            CardMarkdown(text: details).font(.subheadline).foregroundStyle(.secondary)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .accessibilityElement(children: .combine)
            }
        }
    }
}

/// One picture, loaded over https only.
struct ImageBlock: View {
    let source: String
    let alt: String

    var body: some View {
        if !source.isEmpty {
            let shape = RoundedRectangle(cornerRadius: CardMetrics.innerCorner, style: .continuous)
            Group {
                if let url = secureURL(source) {
                    AsyncImage(url: url) { phase in
                        switch phase {
                        case .success(let image):
                            image.resizable().scaledToFit().frame(maxWidth: .infinity, maxHeight: 280)
                        case .failure:
                            placeholder(failed: true)
                        default:
                            placeholder(failed: false)
                        }
                    }
                } else {
                    placeholder(failed: true)
                }
            }
            .clipShape(shape)
            .frame(maxWidth: .infinity)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(alt.isEmpty ? "Picture" : alt)
            .accessibilityAddTraits(.isImage)
        }
    }

    /// The picture's place while it loads, and what is left when it cannot be shown.
    private func placeholder(failed: Bool) -> some View {
        Rectangle().fill(.cardFill)
            .aspectRatio(16 / 9, contentMode: .fit)
            .overlay {
                if failed {
                    VStack(spacing: 6) {
                        Image(systemName: "photo").font(.title3)
                        if !alt.isEmpty { Text(alt).font(.footnote).multilineTextAlignment(.center) }
                    }
                    .foregroundStyle(.secondary)
                    .padding(12)
                }
            }
    }
}

/// A row of short labels.
struct TagsBlock: View {
    let tags: [String]

    var body: some View {
        FlowLayout(spacing: 6, lineSpacing: 6) {
            ForEach(tags.indices, id: \.self) { i in
                Text(tags[i])
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(.cardFill, in: Capsule())
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Likely next questions. A tap sends the item's text as the owner's next message.
struct FollowUpsBlock: View {
    let items: [String]
    @Environment(\.cardHost) private var host

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !items.isEmpty { CardRule() }
            RuledRows(count: items.count) { i in
                Button { host.ask(items[i]) } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        Image(systemName: "arrow.turn.down.right")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(.tertiary)
                            .accessibilityHidden(true)
                        Text(items[i])
                            .font(.subheadline)
                            .multilineTextAlignment(.leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(.vertical, 10)
                    .frame(minHeight: CardMetrics.tap)
                }
                .buttonStyle(RowPressStyle())
                .accessibilityHint("Asks Eve this")
            }
        }
    }
}

/// A link a card may load a picture from: https, nothing else.
func secureURL(_ text: String) -> URL? {
    guard let url = URL(string: text), url.scheme?.lowercased() == "https", url.host != nil else { return nil }
    return url
}
