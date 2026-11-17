// Sends the owner makes while eve is still replying (docs/app-chat.md, "Sending while eve replies").
//
// Owner, 2026-10-26: "I wanted to just be able to double-send or triple-send, and it just catches things", as Claude
// Code queues what is typed while it works. The composer stays a send button while eve replies. Each send that
// cannot start yet waits here, in order, and starts as an ordinary turn once the current reply has ended: finished,
// failed or stopped (so Stop ends the reply and the next one goes). The one wait beyond that: after a stream the app
// cut in the background, the next send starts when the chat shows again (`hiddenAfterError`). Until it starts, the
// thread shows it at its end as a dimmed owner bubble marked "Queued". The moment it starts it leaves this queue, in
// the same step that puts it in the thread, so it is never shown twice.
//
// A queued send never joins the reply that is running: folding a message into a running turn (steering) missed its
// eval bar on 2026-10-26.
import Foundation

public struct QueuedSend: Identifiable, Equatable, Sendable {
    /// The bubble's key in this app ("queued:3"); never a message id.
    public let id: String
    /// What the bubble shows.
    public var text: String
    /// What the daemon is sent, when that is more than the bubble's words (a card tap's context).
    public var wire: String?
    public var attachments: [ChatAttachment]
}

/// What says whether the next send may start now.
public struct StartGate: Equatable, Sendable {
    /// A turn this app started has not settled yet.
    public var inFlight: Bool
    /// The first page of history has arrived.
    public var historyLoaded: Bool
    /// The chat is hidden and the last reply ended in an error. That is a stream the app cut after 25 s in the
    /// background (docs/app-chat.md, "Background and foreground"), and a turn started now could stay open with nobody
    /// reading it. The next send waits until the chat shows again.
    public var hiddenAfterError: Bool

    public init(inFlight: Bool, historyLoaded: Bool, hiddenAfterError: Bool) {
        self.inFlight = inFlight
        self.historyLoaded = historyLoaded
        self.hiddenAfterError = hiddenAfterError
    }

    /// A send may start only when nothing of this app's is running, the thread is loaded, and it would be read.
    public var mayStart: Bool { !inFlight && historyLoaded && !hiddenAfterError }
}

public struct SendQueue: Equatable, Sendable {
    public private(set) var items: [QueuedSend] = []
    private var made = 0

    public init() {}

    /// The owner's send goes to the end of the line.
    @discardableResult
    public mutating func push(_ text: String, wire: String? = nil, attachments: [ChatAttachment] = []) -> QueuedSend {
        made += 1
        let item = QueuedSend(id: "queued:\(made)", text: text, wire: wire, attachments: attachments)
        items.append(item)
        return item
    }

    /// The send to start now, taken off the queue; none while `gate` says wait or nothing waits.
    public mutating func take(_ gate: StartGate) -> QueuedSend? {
        guard !items.isEmpty, gate.mayStart else { return nil }
        return items.removeFirst()
    }
}
