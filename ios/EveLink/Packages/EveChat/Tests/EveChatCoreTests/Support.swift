// Shared by the chat's core tests: made-up lines, a stream chunk, and a thread the ledger writes to.
import Foundation
import XCTest
@testable import EveChatCore

let ID1 = "0123456789abcdef0123456789abcdef"
let ID2 = "fedcba9876543210fedcba9876543210"
let THUMB = "data:image/jpeg;base64,/9j/4AAQSkZJRg=="

nonisolated(unsafe) private var made = 0

func line(_ role: WireMessage.Role, _ text: String, id: String? = nil, t: Double? = nil, kind: String = "say",
          attachments: [ChatAttachment] = [], rid: String? = nil, sources: [ChatSource] = []) -> WireMessage {
    made += 1
    return WireMessage(id: id ?? "m\(made)", role: role, text: text, t: t ?? Double(5000 + made), kind: kind,
                       attachments: attachments, rid: rid, sources: sources)
}

func photo(_ caption: String, _ id: String = ID1, gone: Bool = false) -> WireMessage {
    line(.eve, caption, kind: "media",
         attachments: [ChatAttachment(id: id, name: "001421-0.jpg", kind: .image, thumb: gone ? nil : THUMB, gone: gone)])
}

/// One server-sent event carrying `content`, as the daemon writes it.
func chunk(_ content: String) -> String {
    let body: [String: Any] = ["choices": [["index": 0, "delta": ["content": content]]]]
    return "data: " + String(decoding: try! JSONSerialization.data(withJSONObject: body), as: UTF8.self) + "\n\n"
}

/// A thread the ledger writes to, as the engine's is.
@MainActor
final class ThreadSink: LedgerSink {
    var thread: [ChatMessage] = []
    var idle = true
    var canInsert = true

    func append(_ messages: [ChatMessage]) { thread.append(contentsOf: messages) }

    func insertBeforeReply(_ messages: [ChatMessage]) -> Bool {
        guard canInsert, let last = thread.last, last.role == .eve, thread.contains(where: { $0.role == .owner })
        else { return false }
        thread.insert(contentsOf: messages, at: thread.count - 1)
        return true
    }

    func attachSources(_ sources: [ChatSource], userID: String?) {
        guard let i = ChatLines.replyIndex(in: thread, after: userID) else { return }
        thread[i].sources = ChatSource.merged(thread[i].sources + sources)
    }

    var ids: [String] { thread.map(\.id) }
}
