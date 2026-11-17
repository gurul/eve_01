// A json-render card in the chat: the block of a reply, drawn natively. It keeps the last drawable card while the
// block streams, says so when a finished block cannot be drawn and shows the words it could read, and offers
// Show text / Show card and Reset inputs. The native twin of the web page's JsonCard.tsx.
// Behaviour: docs/app-chat.md, "Using generated cards" and "The card engine".
import SwiftUI
import EveCardCore
import JSONRenderCore
import JSONRenderSwiftUI

/// One card.
///
/// - `stateKey` names this card among all cards (a message and a block): what the owner typed or picked is kept
///   under it while the app runs, so it survives the view being made again.
/// - `code` is what is inside the block's fence, so far.
/// - `streaming` is true while the reply is still arriving.
/// - `onAsk` gets a tap that continues the conversation: the message, and form values when the card sent some.
/// - `onOpen` gets a link to open.
public struct JsonCardView: View {
    private let stateKey: String
    private let code: String
    private let streaming: Bool
    private let onAsk: @MainActor (String, JSONValue?) -> Void
    private let onOpen: @MainActor (String) -> Void

    @State private var session = CardSession()
    @State private var revision = 0
    @State private var textMode = false

    public init(
        stateKey: String,
        code: String,
        streaming: Bool,
        onAsk: @escaping @MainActor (_ message: String, _ data: JSONValue?) -> Void,
        onOpen: @escaping @MainActor (_ url: String) -> Void
    ) {
        self.stateKey = stateKey
        self.code = code
        self.streaming = streaming
        self.onAsk = onAsk
        self.onOpen = onOpen
    }

    /// For tests and previews: the card as it is after a tap on Show text.
    init(stateKey: String, code: String, showingText: Bool) {
        self.init(stateKey: stateKey, code: code, streaming: false, onAsk: { _, _ in }, onOpen: { _ in })
        _textMode = State(initialValue: showingText)
    }

    public var body: some View {
        let read = session.read(code: code, streaming: streaming)
        let show = CardPresentation(current: read.current, selected: read.selected, lastGood: read.lastGood, streaming: streaming, textMode: textMode)
        let _ = connect()
        VStack(alignment: .leading, spacing: 0) {
            if let spec = read.selected?.spec {
                JSONRenderView(
                    spec: spec,
                    registry: session.registry,
                    initialState: session.initialState(stateKey: stateKey),
                    loading: streaming
                )
                .id(revision)
                .environment(\.cardHost, host)
                // The text view covers the card rather than replacing it, so Show card returns to the same live
                // card with its inputs as they were.
                .frame(height: show.hidesCard ? 0 : nil, alignment: .top)
                .clipped()
                .opacity(show.hidesCard ? 0 : 1)
                .allowsHitTesting(!show.hidesCard)
                .accessibilityHidden(show.hidesCard)
            }
            if let status = show.status {
                Text(status)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.vertical, 8)
                    .accessibilityAddTraits(.updatesFrequently)
            }
            if let text = show.text {
                VStack(alignment: .leading, spacing: 8) {
                    Text(show.caption ?? "").font(.footnote).foregroundStyle(.secondary)
                    Text(text).font(.body).textSelection(.enabled)
                }
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(CardMetrics.padding)
                .background(.cardSurface, in: RoundedRectangle(cornerRadius: CardMetrics.corner, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: CardMetrics.corner, style: .continuous).strokeBorder(.cardEdge, lineWidth: 1))
            }
            if show.toggle != nil || show.offersReset {
                HStack(spacing: 20) {
                    if let toggle = show.toggle {
                        Button(toggle) { textMode.toggle() }
                    }
                    if show.offersReset {
                        Button("Reset inputs") {
                            FormStateCache.shared.delete(stateKey)
                            revision += 1
                        }
                    }
                }
                .buttonStyle(QuietButtonStyle())
                .padding(.horizontal, 4)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        // A card is as tall as its parts at the width it is given, whatever height it is offered: nothing in it
        // is squeezed to fit.
        .fixedSize(horizontal: false, vertical: true)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Eve card")
    }

    private var host: CardHost {
        CardHost(
            ask: { [onAsk] text in onAsk(text, nil) },
            open: onOpen,
            streaming: streaming,
            stateChanged: { [session, stateKey] state in session.stateChanged(state, stateKey: stateKey) }
        )
    }

    /// Points the card's actions at this view's closures. The registry's handlers are made once and outlive a redraw.
    private func connect() {
        session.actions.ask = onAsk
        session.actions.open = onOpen
    }
}

/// A small text button under the card.
private struct QuietButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.footnote.weight(.medium))
            .foregroundStyle(.secondary)
            .frame(minHeight: CardMetrics.tap)
            .contentShape(Rectangle())
            .opacity(configuration.isPressed ? 0.5 : 1)
    }
}
