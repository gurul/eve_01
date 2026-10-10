import Foundation

@main
enum NoteStoreTests {
    static func main() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let memory = root.appendingPathComponent("memory.jsonl")
        let encoder = JSONEncoder()
        let records = [
            Thought(id: 1, ts: 1, yaw: 0, pitch: 0, thought: "oldest"),
            Thought(id: 2, ts: 2, yaw: 0, pitch: 0, thought: "☕ café 🦎\nsecond line"),
            Thought(id: 3, ts: 3, yaw: 0, pitch: 0, thought: String(repeating: "界", count: 600)),
        ]
        let lines = try records.map { String(decoding: try encoder.encode($0), as: UTF8.self) }
        for trailingNewline in [false, true] {
            let text = lines[0] + "\n\nmalformed\n" + lines[1] + "\n" + lines[2]
                + "\n{incomplete" + (trailingNewline ? "\n" : "")
            try Data(text.utf8).write(to: memory)
            for chunk in [1, 2, 3, 7, 64, 65536] {
                for count in [1, 2, 3, 20] {
                    let found = NoteStore.readThoughts(memory, limit: count, chunkBytes: chunk)
                    precondition(found == Array(records.reversed().prefix(count)), "reverse reader: chunk \(chunk), count \(count)")
                }
            }
        }
        precondition(NoteStore.readThoughts(memory, limit: 0).isEmpty)
        precondition(NoteStore.readThoughts(memory, limit: -1).isEmpty)
        precondition(NoteStore.readThoughts(memory, chunkBytes: 0).isEmpty)
        precondition(NoteStore.readThoughts(root.appendingPathComponent("missing")).isEmpty)
        try Data().write(to: memory)
        precondition(NoteStore.readThoughts(memory).isEmpty)
        try Data(lines[0].utf8).write(to: memory)
        precondition(NoteStore.readThoughts(memory, chunkBytes: 2) == [records[0]])

        let day = root.appendingPathComponent("transcripts/meetings/2026-10-03", isDirectory: true)
        try FileManager.default.createDirectory(at: day, withIntermediateDirectories: true)
        let meeting = day.appendingPathComponent("093000-session.md")
        let header = "# Café ☕\nwords: 420\n\nA decision about 🦎.\n"
            + String(repeating: "\n", count: 36)
        let transcript = String(repeating: "unshown transcript words\n", count: 50000)
        try Data((header + transcript).utf8).write(to: meeting)
        for chunk in [1, 2, 3, 7, 8192] {
            precondition(NoteStore.readRoomNoteHead(meeting, chunkBytes: chunk) == header)
        }
        let notes = NoteStore.readRoomNotes(store: root)
        precondition(notes.count == 1)
        precondition(notes[0].title == "Café ☕" && notes[0].gist == "A decision about 🦎.")
        precondition(notes[0].words == 420 && notes[0].time == "09:30")
        precondition(NoteStore.readRoomNotes(store: root, limit: 0).isEmpty)
        precondition(NoteStore.readRoomNotes(store: root, limit: -1).isEmpty)
        try Data("# Short\nOne line".utf8).write(to: meeting)
        precondition(NoteStore.readRoomNoteHead(meeting, chunkBytes: 2) == "# Short\nOne line")
        print("NOTE_STORE_TESTS_OK: reverse chunks, malformed rows, Unicode, header limits, missing/empty files")
    }
}
