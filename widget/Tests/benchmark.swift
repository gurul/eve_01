import Foundation

@main
enum NoteStoreBenchmark {
    static func main() throws {
        let args = CommandLine.arguments
        precondition(args.count == 3, "usage: benchmark --prepare|label directory")
        let root = URL(fileURLWithPath: args[2], isDirectory: true)
        if args[1] == "--prepare" {
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            let memory = root.appendingPathComponent("memory.jsonl")
            precondition(FileManager.default.createFile(atPath: memory.path, contents: nil))
            let writer = try FileHandle(forWritingTo: memory)
            let encoder = JSONEncoder()
            for id in 0..<30000 {
                var line = try encoder.encode(Thought(id: id, ts: Double(id), yaw: 0, pitch: 0,
                                                     thought: String(repeating: "history ", count: 256)))
                line.append(0x0A)
                try writer.write(contentsOf: line)
            }
            try writer.close()
            let day = root.appendingPathComponent("transcripts/meetings/2026-10-03", isDirectory: true)
            try FileManager.default.createDirectory(at: day, withIntermediateDirectories: true)
            let header = "# Benchmark\nwords: 500000\n\nA decision.\n" + String(repeating: "\n", count: 36)
            try Data((header + String(repeating: "transcript words that are not shown\n", count: 500000)).utf8)
                .write(to: day.appendingPathComponent("093000-benchmark.md"))
            return
        }
        var times: [Double] = []
        for _ in 0..<7 {
            let start = DispatchTime.now().uptimeNanoseconds
            let result = NoteStore.read(notesDir: root, store: root).snapshot
            let elapsed = Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000
            precondition(result.thoughts.map(\.id) == Array((29800..<30000).reversed()))
            precondition(result.roomNotes.count == 1 && result.roomNotes[0].gist == "A decision.")
            times.append(elapsed)
        }
        let bytes = try [root.appendingPathComponent("memory.jsonl"),
                         root.appendingPathComponent("transcripts/meetings/2026-10-03/093000-benchmark.md")]
            .reduce(0) { $0 + (try $1.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0) }
        let value: [String: Any] = ["label": args[1], "fixture_bytes": bytes, "samples_ms": times,
                                    "median_ms": times.sorted()[times.count / 2], "validated_records": 200]
        print(String(decoding: try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self))
    }
}
