// The Updates pane (docs/updates.md), left of the Chat: things to act on that eve found in the owner's mail, calendar,
// notes, memory and projects, and the apps connected through Composio (Slack, Notion, Linear, work mail and calendar).
// Owner, 2026-10-12: "just actionable things", no weather, "when I open, chat should still be main pane". Each card
// reads as one sentence, a verb chip then the rest. The hold menu's Send to chat sends exactly that sentence (and where
// it came from) to the chat, so nothing the owner cannot see goes to eve; Open in <App> opens the source's own page
// when the card has one; Dismiss takes a card away for good. A plain tap does nothing; a sideways swipe changes the pane.
import SwiftUI

@MainActor
final class UpdatesModel: ObservableObject {
    @Published private(set) var list = UpdatesList()
    @Published private(set) var problem: String?
    /// A dismiss the daemon refused: one line over the list.
    @Published private(set) var dismissProblem: String?
    /// A pull to refresh is running: the POST and the reads after it.
    @Published private(set) var refreshing = false
    /// A dismiss landed: a light haptic.
    @Published private(set) var dismissedCount = 0

    private let client: WorkClient
    /// The reads while the daemon says it is rebuilding (on its own, not after a pull): one at a time.
    private var poll: Task<Void, Never>?

    /// How long the pane waits on a rebuild before it stops asking (the daemon keeps going; the next open shows it).
    static let pollLimit: Duration = .seconds(60)
    static let pollEvery: Duration = .seconds(2)

    /// Its own client on the pairing, as WorkModel's (WorkClient.swift's calls, retried at eve's new address).
    init(relay: Relay) {
        client = WorkClient(
            endpoint: { [weak relay] in relay?.pairing.map { ChatEndpoint(base: $0.base, token: $0.token) } },
            refresh: { [weak relay] in await relay?.refreshAddress() ?? false })
    }

    var cards: [UpdateCard]? { list.cards }
    /// Rebuilding on the Mac, by the owner's pull or on its own.
    var busy: Bool { refreshing || list.refreshing }

    /// One read. While the daemon is rebuilding, keep reading in the background until it is done.
    func load() async {
        await read()
        if list.refreshing, !refreshing { startPolling() }
    }

    /// Pull to refresh: ask for a rebuild, then read until it is done (bounded). Never waits on the model itself.
    func refresh() async {
        guard !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        stop()                                  // this pull reads until done itself
        problem = nil
        switch await client.refreshUpdates() {
        case .success:
            await pollUntilDone()
        case .failure(let f):
            problem = f.message
        }
    }

    /// Off the list at once; back in its place if the daemon refuses. Either way the list is read again after: the
    /// daemon dismisses the card's sources for good, so another card citing only them goes too (one that also cites
    /// something else stays), and a card it no longer knows is shown only if it is still served.
    func dismiss(_ card: UpdateCard) {
        guard let removed = list.dismiss(card.id) else { return }
        dismissProblem = nil
        Task {
            let ok: Bool
            switch await client.dismissUpdate(card.id) {
            case .success(let ack): ok = ack.ok
            case .failure: ok = false
            }
            if ok {
                dismissedCount += 1
            } else {
                list.restore(removed.card, at: removed.index)
                dismissProblem = "Couldn't dismiss that. Try again."
            }
            await read()
        }
    }

    func stop() {
        poll?.cancel()
        poll = nil
    }

    private func startPolling() {
        guard poll == nil else { return }
        poll = Task { [weak self] in
            await self?.pollUntilDone()
            if !Task.isCancelled { self?.poll = nil }     // a cancelled one was replaced or stopped already
        }
    }

    private func pollUntilDone() async {
        let until = ContinuousClock.now + Self.pollLimit
        repeat {
            await read()
            guard list.refreshing, problem == nil, !Task.isCancelled, ContinuousClock.now < until else { return }
            try? await Task.sleep(for: Self.pollEvery)
        } while !Task.isCancelled
    }

    private func read() async {
        let n = list.nextRead()
        switch await client.updates() {
        case .success(let feed):
            if list.apply(feed, read: n) { problem = nil }
        case .failure(let f):
            problem = f.message
        }
    }
}

struct UpdatesView: View {
    @ObservedObject var model: UpdatesModel
    /// A tapped card: the screen switches to Chat and sends its tap text. false while a reply is still streaming
    /// there (nothing is sent; the pane says to wait).
    let ask: (UpdateCard) -> Bool
    /// The last tap found eve still answering.
    @State private var askWaits = false
    /// A https link opens its app when the app claims it (Slack, Notion), Safari otherwise.
    @Environment(\.openURL) private var openURL

    var body: some View {
        VStack(spacing: 0) {
            bar
            List {
                if let line = model.dismissProblem { note(line, .red) }
                if askWaits, model.cards?.isEmpty == false {
                    note("eve is still answering in Chat. Tap the card again when it's done.", .secondary)
                }
                rows
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, 0)
        }
        .background(Color.eveBackground)
        .refreshable { await model.refresh() }
        .task { await model.load() }
        .onDisappear {
            model.stop()
            askWaits = false
        }
        .sensoryFeedback(.impact(weight: .light), trigger: model.dismissedCount)
    }

    private var bar: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 1) {
                Text("Updates").font(.headline)
                TimelineView(.periodic(from: .now, by: 60)) { tl in
                    if let age = UpdatesAge.label(model.list.generatedAt, now: tl.date) {
                        Text(model.busy ? "Looking for new updates…" : age)
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            Spacer()
            if model.busy {
                ProgressView()
                    .controlSize(.small)
                    .frame(width: 36, height: 36)
                    .accessibilityLabel("Looking for new updates")
            } else {
                Button { Task { await model.refresh() } } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.body.weight(.medium))
                        .frame(width: 36, height: 36)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .accessibilityLabel("Refresh updates")
            }
        }
        .frame(minHeight: 36)
        .padding(.horizontal, 20)
        .padding(.top, 8)
        .padding(.bottom, 4)
    }

    @ViewBuilder private var rows: some View {
        if let cards = model.cards, !cards.isEmpty {
            ForEach(cards) { card in
                // A tap reads; sending is a choice in the hold menu (owner, 2026-10-15: a tap sent cards by accident).
                UpdateCardView(card: card)
                    .contentShape(Rectangle())
                    .listRowBackground(Color.eveBackground)
                    .listRowSeparator(.hidden)
                    .listRowInsets(EdgeInsets(top: 5, leading: 16, bottom: 5, trailing: 16))
                    .contextMenu {
                        Button("Send to chat", systemImage: "arrow.up.message") { askWaits = !ask(card) }
                        if let app = UpdateLink.app(card.link) {
                            Button("Open in \(app.name)", systemImage: "arrow.up.forward.app") { openURL(app.url) }
                        }
                        Button("Dismiss", systemImage: "xmark") { model.dismiss(card) }
                    } preview: {
                        UpdateCardView(card: card).frame(width: 340)
                    }
                    .accessibilityHint(UpdateLink.app(card.link) == nil
                                       ? "Touch and hold to send it to eve in Chat or dismiss it"
                                       : "Touch and hold to send it to eve in Chat, open it, or dismiss it")
                    .accessibilityAction(named: "Send to chat") { askWaits = !ask(card) }
                    .accessibilityActions {
                        if let app = UpdateLink.app(card.link) {
                            Button("Open in \(app.name)") { openURL(app.url) }
                        }
                    }
                    .accessibilityAction(named: "Dismiss") { model.dismiss(card) }
            }
        } else if let problem = model.problem {
            VStack(alignment: .leading, spacing: 10) {
                Text(problem).font(.subheadline).foregroundStyle(.secondary)
                Button("Try again") { Task { await model.load() } }
                    .font(.subheadline.weight(.medium))
                    .buttonStyle(.bordered)
            }
            .padding(.vertical, 6)
            .listRowBackground(Color.eveBackground)
            .listRowSeparator(.hidden)
        } else if model.cards == nil {
            quiet(model.busy ? "Looking through your mail, calendar and notes…" : "Loading…")
        } else {
            quiet(model.busy ? "Looking through your mail, calendar and notes…" : "Nothing to act on right now.")
        }
        if model.cards?.isEmpty == false, let problem = model.problem {
            note(problem, .secondary)              // stale cards stay; say why they may be old
        }
    }

    private func quiet(_ line: String) -> some View {
        Text(line)
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .padding(.vertical, 6)
            .listRowBackground(Color.eveBackground)
            .listRowSeparator(.hidden)
    }

    private func note(_ line: String, _ color: Color) -> some View {
        Text(line)
            .font(.footnote)
            .foregroundStyle(color)
            .listRowBackground(Color.eveBackground)
            .listRowSeparator(.hidden)
    }
}

/// One card: the verb chip and the sentence as one wrapping line of text, then a footnote with the source's symbol, its
/// title and the due day.
struct UpdateCardView: View {
    let card: UpdateCard
    @Environment(\.colorScheme) private var scheme

    private var due: String? { UpdateDue.label(card.due) }
    private var pressing: Bool { UpdateDue.isPressing(card.due) }
    private var symbol: String { card.knownKind?.symbol ?? "sparkles" }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            sentence
            if !card.source.isEmpty || due != nil {
                HStack(spacing: 6) {
                    Image(systemName: symbol).imageScale(.small)
                    if !card.source.isEmpty {
                        // wraps, never cut: the tap sends the whole source, so all of it is on the card
                        Text(card.source).fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer(minLength: 8)
                    if let due {
                        Text(due)
                            .fontWeight(pressing ? .semibold : .regular)
                            .foregroundStyle(pressing ? Color.orange : Color.secondary)
                            .layoutPriority(1)
                    }
                }
                .font(.footnote)
                .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 13)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 20, style: .continuous).fill(Color.eveFill))
        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(Color.eveEdge, lineWidth: 1))
        .contentShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityText)
        .accessibilityAddTraits(.isButton)
    }

    /// "[Review] your storage before billing starts": the chip is drawn behind the verb's run (ChipRenderer), so the
    /// sentence wraps as text does, under the chip too. An en space and a space after the verb leave room for the chip's edge.
    private var sentence: some View {
        (Text(card.verb).fontWeight(.semibold).customAttribute(VerbChip()) + Text("\u{2002} " + card.text))
            .font(.body)
            .foregroundStyle(.primary)
            .fixedSize(horizontal: false, vertical: true)
            .textRenderer(ChipRenderer(fill: Color.accentColor.opacity(scheme == .dark ? 0.28 : 0.14)))
            .padding(.leading, ChipRenderer.inset)
            .padding(.vertical, 2)
    }

    private var accessibilityText: String {
        var parts = ["\(card.verb) \(card.text)"]
        if !card.source.isEmpty { parts.append("From " + (card.knownKind.map { $0.from(card.source) } ?? card.source)) }
        if let due { parts.append(due == "Overdue" ? "Overdue" : "Due \(due)") }
        return parts.joined(separator: ". ")
    }
}

/// Marks the verb's run in the card's text.
private struct VerbChip: TextAttribute {}

/// Draws a capsule behind the runs marked VerbChip, then the text over it.
private struct ChipRenderer: TextRenderer {
    let fill: Color
    /// How far the chip reaches past the verb on each side (the card's text is padded by as much on the left).
    static let inset: CGFloat = 7

    func draw(layout: Text.Layout, in ctx: inout GraphicsContext) {
        for line in layout {
            for run in line where run[VerbChip.self] != nil {
                let r = run.typographicBounds.rect.insetBy(dx: -Self.inset, dy: -2)
                ctx.fill(Path(roundedRect: r, cornerRadius: r.height / 2, style: .continuous), with: .color(fill))
            }
            for run in line { ctx.draw(run) }
        }
    }
}

