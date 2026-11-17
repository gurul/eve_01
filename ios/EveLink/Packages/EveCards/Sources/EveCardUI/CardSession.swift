// What one card on screen remembers between redraws: the last drawable program while a block streams, the
// registry its renderer uses, and what the owner typed or picked. Also the plain rules for what the card shows
// around the drawing (a status line, the text view, the two small buttons), kept apart from the views so tests
// can read them. A port of the web page's JsonCard.tsx. Behaviour: docs/app-chat.md, "Using generated cards".
import SwiftUI
import EveCardCore
import JSONRenderCore

/// What the owner typed or picked, by card and by state path. In memory only: never on disk, never in a log.
/// It keeps the 100 most recently updated cards.
@MainActor
final class FormStateCache {
    static let shared = FormStateCache()

    private let limit: Int
    private var states: [String: [String: JSONValue]] = [:]
    /// Keys, least recently updated first.
    private var order: [String] = []

    init(limit: Int = 100) {
        self.limit = limit
    }

    var count: Int { states.count }

    func get(_ key: String) -> [String: JSONValue]? { states[key] }

    func delete(_ key: String) {
        guard states.removeValue(forKey: key) != nil else { return }
        order.removeAll { $0 == key }
    }

    func set(_ key: String, _ state: [String: JSONValue]) {
        delete(key)
        states[key] = state
        order.append(key)
        while order.count > limit { states.removeValue(forKey: order.removeFirst()) }
    }
}

/// The values in `current` that differ from the block's own `seed`, by JSON Pointer path. Objects are compared
/// member by member; anything else is one value.
func stateEdits(seed: JSONValue?, current: JSONValue, path: String = "") -> [String: JSONValue] {
    if case .object(let now) = current, case .object(let was)? = seed ?? (path.isEmpty ? .object([:]) : nil) {
        var out: [String: JSONValue] = [:]
        for (key, value) in now {
            let step = key.replacingOccurrences(of: "~", with: "~0").replacingOccurrences(of: "/", with: "~1")
            out.merge(stateEdits(seed: was[key], current: value, path: "\(path)/\(step)")) { _, new in new }
        }
        return out
    }
    return seed == current ? [:] : [path: current]
}

/// The block's own state with the owner's edits on top.
func stateWithEdits(seed: [String: JSONValue]?, edits: [String: JSONValue]) -> [String: JSONValue] {
    var state = JSONValue.object(seed ?? [:])
    for path in edits.keys.sorted(by: { ($0.count, $0) < ($1.count, $1) }) where !path.isEmpty {
        if let value = edits[path] { state = setByPath(state, path, value) }
    }
    return state.objectValue ?? seed ?? [:]
}

/// What the card shows around its drawing, decided from the block alone.
struct CardPresentation: Equatable {
    /// The program to draw, when there is one.
    var drawsCard: Bool
    /// The drawing is hidden behind the text view, but kept, with its inputs.
    var hidesCard: Bool
    /// A line under the card: still building, or could not be displayed.
    var status: String?
    /// The text view: a caption and the words of the card.
    var caption: String?
    var text: String?
    /// The Show text / Show card button's title, when the button is offered.
    var toggle: String?
    var offersReset: Bool

    static let building = "Building your card…"
    static let failedWithText = "This card couldn’t be displayed. Recovered text may be incomplete."
    static let failedEmpty = "This card couldn’t be displayed. Ask Eve to send it as text."

    init(current: CardProgram, selected: CardProgram?, lastGood: CardProgram?, streaming: Bool, textMode: Bool) {
        let failed = !streaming && selected == nil
        let recovered = !current.text.isEmpty ? current.text : lastGood?.text ?? ""
        let showText = failed || textMode
        drawsCard = selected?.spec != nil
        hidesCard = showText
        if streaming && selected == nil {
            status = Self.building
        } else if failed {
            status = recovered.isEmpty ? Self.failedEmpty : Self.failedWithText
        }
        if showText && !recovered.isEmpty {
            caption = "Text from this reply" + (current.hasControls ? " · live values aren’t included" : "")
            text = recovered
        }
        if !streaming && !failed && !recovered.isEmpty { toggle = textMode ? "Show card" : "Show text" }
        offersReset = !streaming && !failed && (selected?.hasControls ?? false)
    }
}

/// One card's memory. It lives as long as the card's view does.
@MainActor
final class CardSession {
    let actions = CardActions()
    private(set) lazy var registry = CardRegistry.make(actions: actions)

    private var code: String?
    private var streaming = false
    private var current: CardProgram?
    private var selected: CardProgram?
    /// The last program that could be drawn, as it stood before the block now on screen was read.
    private var lastGood: CardProgram?
    private var newest: CardProgram?

    /// Reads the block, once per change, and picks what to draw: what has arrived so far while it streams (the
    /// last good card through a bad chunk), and only a whole card once it is finished.
    func read(code: String, streaming: Bool) -> (current: CardProgram, selected: CardProgram?, lastGood: CardProgram?) {
        if let current, self.code == code, self.streaming == streaming { return (current, selected, lastGood) }
        let program = inspectCard(code, streaming: streaming)
        lastGood = newest
        let pick = selectCard(current: program, previous: newest, streaming: streaming)
        if let pick { newest = pick }
        self.code = code
        self.streaming = streaming
        current = program
        selected = pick
        return (program, pick, lastGood)
    }

    /// The state the renderer starts from: the block's own, with what the owner typed or picked on top.
    func initialState(stateKey: String) -> [String: JSONValue]? {
        guard let edits = FormStateCache.shared.get(stateKey), !edits.isEmpty else { return nil }
        return stateWithEdits(seed: selected?.spec?.state, edits: edits)
    }

    /// Keeps what differs from the block's own state after a change.
    func stateChanged(_ state: JSONValue, stateKey: String) {
        let edits = stateEdits(seed: selected?.spec?.state.map(JSONValue.object), current: state)
        if edits.isEmpty { FormStateCache.shared.delete(stateKey) } else { FormStateCache.shared.set(stateKey, edits) }
    }
}
