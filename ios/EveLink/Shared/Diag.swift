// The diagnostics log: one line per step of a press (the intent ran, the decision, the microphone, the Live Activity,
// the call), shown at the bottom of the app and printed for `devicectl ... --console`. Added 2026-10-04 when the
// first Action Button test "didn't work" and the app said nothing about why. Never logs the token or audio.
import Foundation
import os

@MainActor
final class Diag: ObservableObject {
    static let shared = Diag()
    static let keep = 60
    @Published private(set) var lines: [String] = []
    private let logger = Logger(subsystem: "com.github.eve-bridge.EveLink", category: "talk")
    private let clock: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss.SSS"
        return f
    }()

    func add(_ line: String) {
        let stamped = "\(clock.string(from: Date())) \(line)"
        lines.append(stamped)
        if lines.count > Self.keep { lines.removeFirst(lines.count - Self.keep) }
        logger.notice("\(line, privacy: .public)")
        print("[evelink] \(stamped)")
    }

    func clear() { lines.removeAll() }
}

/// One diagnostics line, from anywhere. Written to Library/Caches/diag.log at once, on the caller's thread, so the
/// last steps before a crash survive it (2026-10-04: the first Action Button presses crashed inside App Intents and
/// the in-memory lines died with the process). Read it with devicectl ... copy from --domain-type appDataContainer.
nonisolated func diag(_ line: String) {
    DiagFile.append(line)
    Task { @MainActor in Diag.shared.add(line) }
}

enum DiagFile {
    private static let lock = NSLock()
    nonisolated(unsafe) private static var handle: FileHandle?
    static let maxBytes: UInt64 = 256 * 1024

    static var url: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("diag.log")
    }

    static func append(_ line: String) {
        lock.lock()
        defer { lock.unlock() }
        let stamp = ISO8601DateFormatter.string(from: Date(), timeZone: .current,
                                                formatOptions: [.withTime, .withColonSeparatorInTime,
                                                                .withFractionalSeconds])
        let pid = ProcessInfo.processInfo.processIdentifier
        guard let data = "\(stamp) [\(pid)] \(line)\n".data(using: .utf8) else { return }
        if handle == nil {
            if !FileManager.default.fileExists(atPath: url.path) {
                FileManager.default.createFile(atPath: url.path, contents: nil)
            }
            handle = try? FileHandle(forWritingTo: url)
        }
        guard let handle else { return }
        if let size = try? handle.seekToEnd(), size > maxBytes {
            try? handle.truncate(atOffset: 0)
        }
        try? handle.write(contentsOf: data)
        try? handle.synchronize()
    }
}
