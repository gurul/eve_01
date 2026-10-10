// Shared between the helper app and the widget extension.
//
// Data flow: eve-bridge writes, under `~/.config/eve-bridge/notes/`:
//   YYYY-MM-DD.md   diary lines `- HH:MM yaw=+20 pitch=40 — <thought>` and, at
//                   night, a `## Evening reflection` section of `- insight` lines
//   memory.jsonl    one record per frame (bridge/src/eve_bridge/diary.py):
//                   thought, observations, what changed, tags, novelty,
//                   importance, whether it was written, valence/arousal/label
//   profile.md      ROOM / HUMAN / SELF / RULES blocks eve rewrites nightly
//   photos/YYYY-MM-DD/HHMMSS-<id>.jpg  the pictures eve kept of views it
//                   found cool (bridge/src/eve_bridge/photos.py); a record
//                   references one by a path relative to the notes directory
// and, under its memory folder `~/.config/eve-bridge/memory/` (records.py):
//   records/starred.md       `- claim (YYYY-MM-DD)`: what the owner said to remember
//   records/days/<day>.md    the dream journal, one page per day eve talked
//   transcripts/meetings/<day>/*.md  recordings of the room eve was asked to take
// The (unsandboxed) helper mirrors all of it into `notes.json` inside the App
// Group container — and copies the referenced photos in beside it, because the
// (sandboxed) widget can read nothing but that container.

import Foundation

enum AppGroup {
    /// macOS App Groups must be prefixed with the Team ID.
    static let id = "SJ8BKXTNUS.com.github.eve-bridge"
    static let fileName = "notes.json"
    /// Tapping the widget opens this in the helper app (Info.plist CFBundleURLTypes).
    static let diaryURL = URL(string: "stackchan://diary")!
    /// The chat window (ChatView.swift in the helper): where tapping the diary widget goes.
    static let chatURL = URL(string: "stackchan://chat")!

    static var containerURL: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: id)
    }

    static var notesFileURL: URL? {
        containerURL?.appendingPathComponent(fileName, isDirectory: false)
    }

    /// Mirrored photos live here, under the same relative paths the records use.
    static var photosURL: URL? {
        containerURL?.appendingPathComponent("photos", isDirectory: true)
    }

    /// The mirrored copy of a record's photo, or nil when it has not been copied.
    static func photo(_ relPath: String?) -> URL? {
        guard let relPath, !relPath.isEmpty, let container = containerURL else { return nil }
        let url = container.appendingPathComponent(relPath, isDirectory: false)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }
}

struct Note: Codable, Hashable, Identifiable, Sendable {
    /// `YYYY-MM-DD`, taken from the daily file name.
    let date: String
    /// `HH:MM` as written by the daemon.
    let time: String
    let yaw: Double?
    let pitch: Double?
    let text: String

    var id: String { "\(date)T\(time) \(text)" }

    /// `yaw=+20 pitch=40` rendered for the dim secondary line.
    var pose: String? {
        var parts: [String] = []
        if let yaw { parts.append("yaw \(Self.signed(yaw))") }
        if let pitch { parts.append("pitch \(Self.signed(pitch))") }
        return parts.isEmpty ? nil : parts.joined(separator: "  ")
    }

    private static func signed(_ v: Double) -> String {
        let rounded = v.rounded()
        let s = rounded == v ? String(Int(rounded)) : String(format: "%.1f", v)
        return v > 0 ? "+\(s)" : s
    }
}

/// One memory record from `memory.jsonl` — everything behind a diary line.
struct Thought: Codable, Hashable, Identifiable, Sendable {
    let id: Int
    let ts: Double
    let yaw: Int
    let pitch: Int
    let thought: String
    var observations: [String] = []
    var changed: [String] = []
    var tags: [String] = []
    var novelty: Int = 5
    var importance: Int = 3
    var written: Bool = false
    var valence: Int = 0
    var arousal: Int = 0
    var label: String = "calm"
    /// `photos/YYYY-MM-DD/HHMMSS-<id>.jpg`, relative to the notes directory —
    /// set when eve found this view cool enough to photograph.
    var photo: String? = nil

    var date: Date { Date(timeIntervalSince1970: ts) }
    var day: String {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: date)
    }
    var time: String {
        let f = DateFormatter()
        f.dateFormat = "HH:mm"
        return f.string(from: date)
    }

    enum CodingKeys: String, CodingKey {
        case id, ts, yaw, pitch, thought, observations, changed, tags, novelty, importance, written, valence,
             arousal, label, photo
    }

    init(id: Int, ts: Double, yaw: Int, pitch: Int, thought: String, observations: [String] = [], changed: [String] = [],
         tags: [String] = [], novelty: Int = 5, importance: Int = 3, written: Bool = false, valence: Int = 0,
         arousal: Int = 0, label: String = "calm", photo: String? = nil) {
        self.id = id; self.ts = ts; self.yaw = yaw; self.pitch = pitch; self.thought = thought
        self.observations = observations; self.changed = changed; self.tags = tags; self.novelty = novelty
        self.importance = importance; self.written = written; self.valence = valence; self.arousal = arousal
        self.label = label; self.photo = photo
    }

    /// Tolerant decoding: the daemon may add fields; older lines may lack some.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int.self, forKey: .id)
        ts = try c.decode(Double.self, forKey: .ts)
        yaw = (try? c.decode(Int.self, forKey: .yaw)) ?? 0
        pitch = (try? c.decode(Int.self, forKey: .pitch)) ?? 45
        thought = try c.decode(String.self, forKey: .thought)
        observations = (try? c.decode([String].self, forKey: .observations)) ?? []
        changed = (try? c.decode([String].self, forKey: .changed)) ?? []
        tags = (try? c.decode([String].self, forKey: .tags)) ?? []
        novelty = (try? c.decode(Int.self, forKey: .novelty)) ?? 5
        importance = (try? c.decode(Int.self, forKey: .importance)) ?? 3
        written = (try? c.decode(Bool.self, forKey: .written)) ?? false
        valence = (try? c.decode(Int.self, forKey: .valence)) ?? 0
        arousal = (try? c.decode(Int.self, forKey: .arousal)) ?? 0
        label = (try? c.decode(String.self, forKey: .label)) ?? "calm"
        photo = try? c.decodeIfPresent(String.self, forKey: .photo)
    }
}

/// A day's `## Evening reflection` insights.
struct Reflection: Codable, Hashable, Identifiable, Sendable {
    let date: String
    let insights: [String]
    var id: String { date }
}

/// One day of talking with its owner, as eve wrote it down in its dream journal.
///
/// The spoken half of eve's memory lives under its memory folder
/// (`~/.config/eve-bridge/memory`): the nightly dream (dream.py) writes one
/// journal page per day eve talked, `records/days/<day>.md`. It is kept apart
/// from `Note`, which is what eve SAW: what the owner said is quotable, what a
/// camera suggested is not.
struct Conversation: Codable, Hashable, Identifiable, Sendable {
    let date: String          // yyyy-MM-dd
    /// A journal is one page for the whole day, written the night after, so it
    /// has no clock time: always `NoteStore.noTime`.
    let time: String
    let title: String
    /// Debts of eve's own, verbatim: "eve owes an answer about the servo".
    var owes: [String] = []

    var id: String { "\(date) \(time) \(title)" }
}

/// A recording eve made of the room, on request (notes.py).
///
/// Unlike a `Conversation`, which is eve's memory of talking WITH its owner,
/// this is a transcript of a meeting, a lecture or a call. The file on disk holds
/// the write-up and the full transcript; this carries enough to list it and open
/// or export the file.
struct RoomNote: Codable, Hashable, Identifiable, Sendable {
    let date: String          // yyyy-MM-dd
    let time: String          // HH:mm
    let title: String
    var gist: String = ""
    var words: Int = 0
    var path: String = ""     // the file, for Reveal in Finder and for export

    var id: String { path.isEmpty ? "\(date) \(time) \(title)" : path }
}

struct NotesSnapshot: Codable, Sendable {
    var updatedAt: Date
    var notesDir: String
    /// Newest first, capped at `NoteStore.limit` by the helper.
    var notes: [Note]
    /// Memory records, newest first, capped at `NoteStore.limit`.
    var thoughts: [Thought] = []
    /// `profile.md`, or empty until eve has reflected once.
    var profile: String = ""
    /// Newest day first.
    var reflections: [Reflection] = []
    /// `highlights.md`: what the human starred from the diary, in file order (oldest first).
    var highlights: [String] = []
    /// Conversations, newest first. What eve heard.
    var conversations: [Conversation] = []
    /// Claims the owner promoted with "remember that" (`records/starred.md`), oldest first.
    var spokenStars: [String] = []
    /// Recordings of the room, newest first.
    var roomNotes: [RoomNote] = []

    enum CodingKeys: String, CodingKey {
        case updatedAt, notesDir, notes, thoughts, profile, reflections, highlights, conversations,
             spokenStars, roomNotes
    }

    init(updatedAt: Date, notesDir: String, notes: [Note], thoughts: [Thought] = [], profile: String = "",
         reflections: [Reflection] = [], highlights: [String] = [], conversations: [Conversation] = [],
         spokenStars: [String] = [], roomNotes: [RoomNote] = []) {
        self.updatedAt = updatedAt; self.notesDir = notesDir; self.notes = notes
        self.thoughts = thoughts; self.profile = profile; self.reflections = reflections; self.highlights = highlights
        self.conversations = conversations; self.spokenStars = spokenStars
        self.roomNotes = roomNotes
    }

    /// Older mirrors wrote only `notes`; keep reading them.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        updatedAt = try c.decode(Date.self, forKey: .updatedAt)
        notesDir = (try? c.decode(String.self, forKey: .notesDir)) ?? ""
        notes = (try? c.decode([Note].self, forKey: .notes)) ?? []
        thoughts = (try? c.decode([Thought].self, forKey: .thoughts)) ?? []
        profile = (try? c.decode(String.self, forKey: .profile)) ?? ""
        reflections = (try? c.decode([Reflection].self, forKey: .reflections)) ?? []
        highlights = (try? c.decode([String].self, forKey: .highlights)) ?? []
        conversations = (try? c.decode([Conversation].self, forKey: .conversations)) ?? []
        spokenStars = (try? c.decode([String].self, forKey: .spokenStars)) ?? []
        roomNotes = (try? c.decode([RoomNote].self, forKey: .roomNotes)) ?? []
    }

    /// The one line most worth showing on a small card: a debt of eve's own.
    var newestDebt: String? {
        for conversation in conversations {
            if let owed = conversation.owes.first { return owed }
        }
        return nil
    }

    /// The newest feeling, from the newest memory record.
    var mood: Thought? { thoughts.first }

    static let empty = NotesSnapshot(updatedAt: .distantPast, notesDir: "", notes: [])

    static let placeholder: NotesSnapshot = {
        let now = Date().timeIntervalSince1970
        return NotesSnapshot(
            updatedAt: .now,
            notesDir: "",
            notes: [
                Note(date: "2026-09-06", time: "10:42", yaw: 20, pitch: 40, text: "Someone finished the coffee and left the spoon in the blue mug — again."),
                Note(date: "2026-09-06", time: "10:31", yaw: -15, pitch: 10, text: "The blinds are half down at half past ten; my human likes the room dim before lunch."),
                Note(date: "2026-09-06", time: "10:17", yaw: 0, pitch: 0, text: "Third morning in a row the chair is pushed in — when does it get used?"),
            ],
            thoughts: [
                Thought(id: 3, ts: now, yaw: 20, pitch: 40, thought: "Someone finished the coffee and left the spoon in the blue mug — again.",
                        observations: ["blue mug, spoon inside", "keyboard centred"], changed: ["mug empty"], tags: ["mug", "spoon"],
                        novelty: 6, importance: 4, written: true, valence: 30, arousal: 35, label: "curious"),
                Thought(id: 2, ts: now - 660, yaw: -15, pitch: 10, thought: "The blinds are half down at half past ten; my human likes the room dim before lunch.",
                        observations: ["blinds half down"], changed: [], tags: ["blinds"], novelty: 5, importance: 3, written: true,
                        valence: 10, arousal: -10, label: "calm"),
                Thought(id: 1, ts: now - 1500, yaw: 0, pitch: 0, thought: "Third morning in a row the chair is pushed in — when does it get used?",
                        observations: ["chair pushed in"], changed: [], tags: ["chair"], novelty: 7, importance: 5, written: true,
                        valence: 20, arousal: 20, label: "curious"),
            ],
            profile: "## ROOM\nA desk with a blue mug that moves.\n\n## HUMAN\nAway around 13:00 most days.\n\n## SELF\nI am eve.\n\n## RULES\nIgnore lighting.",
            reflections: [Reflection(date: "2026-09-05", insights: ["My human leaves around 13:00 most days (12, 15, 18)."])]
        )
    }()
}

// MARK: - Mood colour (mirrors firmware mood.cpp: hue from valence, brightness from arousal)

enum MoodColor {
    /// RGB 0...1 for a feeling. Positive = green (keen) / cyan (calm), negative = orange,
    /// affection = pink, startled = red; brighter with arousal.
    static func rgb(valence: Int, arousal: Int, label: String) -> (Double, Double, Double) {
        let v = Double(valence) / 100.0, a = Double(arousal) / 100.0
        var hue: Double = v >= 0 ? (a < 0 ? 170 : 120) : 20
        if label == "affection" { hue = 320 }
        if label == "startled" { hue = 0 }
        let sat = min(1.0, 0.4 + 0.5 * abs(v))
        let bri = min(1.0, max(0.35, 0.45 + 0.5 * (a + 1) / 2))
        return hsv(hue, sat, bri)
    }

    static func hsv(_ h: Double, _ s: Double, _ v: Double) -> (Double, Double, Double) {
        let c = v * s
        let x = c * (1 - abs((h / 60).truncatingRemainder(dividingBy: 2) - 1))
        let m = v - c
        var r = 0.0, g = 0.0, b = 0.0
        switch h {
        case ..<60: r = c; g = x
        case ..<120: r = x; g = c
        case ..<180: g = c; b = x
        case ..<240: g = x; b = c
        case ..<300: r = x; b = c
        default: r = c; b = x
        }
        return (r + m, g + m, b + m)
    }

    static func emoji(for label: String) -> String {
        switch label {
        case "curious": "🧐"
        case "happy": "😊"
        case "surprised": "😮"
        case "startled": "😳"
        case "bored": "😑"
        case "lonely": "🥺"
        case "affection": "🥰"
        default: "🙂"
        }
    }
}

enum NoteLine {
    /// Parses one daemon line. Returns nil for headers, blanks, or anything not a note bullet.
    /// Accepted shapes:
    ///   `- 10:42 yaw=+20 pitch=40 — sentence`
    ///   `- 10:42 — sentence`
    ///   `- 10:42 yaw=+20 pitch=40 - sentence`   (ASCII dash fallback)
    static func parse(_ raw: String, date: String) -> Note? {
        var line = raw.trimmingCharacters(in: .whitespaces)
        guard line.hasPrefix("- ") || line.hasPrefix("* ") else { return nil }
        line.removeFirst(2)
        line = line.trimmingCharacters(in: .whitespaces)

        let timeEnd = line.index(line.startIndex, offsetBy: 5, limitedBy: line.endIndex) ?? line.endIndex
        let time = String(line[..<timeEnd])
        guard isTime(time) else { return nil }
        var rest = String(line[timeEnd...]).trimmingCharacters(in: .whitespaces)

        let separators = [" — ", " – ", " -- ", " - "]
        var pose = ""
        var text = rest
        for sep in separators {
            if let r = rest.range(of: sep) {
                pose = String(rest[..<r.lowerBound])
                text = String(rest[r.upperBound...])
                break
            }
        }
        if text == rest, rest.hasPrefix("—") {
            rest.removeFirst()
            text = rest
        }
        text = text.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty else { return nil }

        var yaw: Double?
        var pitch: Double?
        for token in pose.split(separator: " ") {
            if let v = token.split(separator: "=", maxSplits: 1).last, token.hasPrefix("yaw=") {
                yaw = Double(v)
            } else if let v = token.split(separator: "=", maxSplits: 1).last, token.hasPrefix("pitch=") {
                pitch = Double(v)
            }
        }
        return Note(date: date, time: time, yaw: yaw, pitch: pitch, text: text)
    }

    /// The `- insight` lines under a `## Evening reflection` header.
    static func reflections(in body: String) -> [String] {
        var out: [String] = []
        var inSection = false
        for raw in body.split(separator: "\n", omittingEmptySubsequences: true) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("## ") {
                let l = line.lowercased()
                inSection = l.contains("dream") || l.contains("reflection")
                continue
            }
            guard inSection, line.hasPrefix("- ") else { continue }
            let text = line.dropFirst(2).trimmingCharacters(in: .whitespaces)
            if !text.isEmpty { out.append(text) }
        }
        return out
    }

    private static func isTime(_ s: String) -> Bool {
        let chars = Array(s)
        guard chars.count == 5, chars[2] == ":" else { return false }
        return [chars[0], chars[1], chars[3], chars[4]].allSatisfy(\.isNumber)
    }
}

enum NoteStore {
    static let limit = 200

    private static var encoder: JSONEncoder {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .iso8601
        e.outputFormatting = [.prettyPrinted, .sortedKeys]
        return e
    }

    private static var decoder: JSONDecoder {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .iso8601
        return d
    }

    /// Reads the mirrored snapshot from the group container. Missing/corrupt → `.empty`.
    static func load() -> NotesSnapshot {
        guard let url = AppGroup.notesFileURL,
              let data = try? Data(contentsOf: url),
              let snap = try? decoder.decode(NotesSnapshot.self, from: data)
        else { return .empty }
        return snap
    }

    /// How many of the newest photos are mirrored into the container. The
    /// widget shows at most a handful; the diary window reads the originals.
    static let mirroredPhotos = 24

    /// A record's photo path is written by the daemon (photos.py) and must
    /// look exactly like `photos/YYYY-MM-DD/HHMMSS-<id>.jpg`. Anything else is
    /// refused rather than resolved: these strings become file operations, and
    /// a loose one would copy or delete the wrong thing.
    static func isPhotoPath(_ rel: String) -> Bool {
        let parts = rel.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 3, parts[0] == "photos" else { return false }
        guard parts[1].count == 10, parts[1].allSatisfy({ $0.isNumber || $0 == "-" }) else { return false }
        guard parts[2].hasSuffix(".jpg"), !parts[2].hasPrefix(".") else { return false }
        return !rel.contains("..")
    }

    /// Copies the photos the newest records reference into the container, and
    /// removes copies nothing references any more. The container mirrors the
    /// notes directory's own layout, so a record's path resolves the same way
    /// on both sides. Failures are not fatal: a missing copy just means no
    /// thumbnail in the widget.
    @discardableResult
    static func mirrorPhotos(_ thoughts: [Thought], notesDir: URL) -> Int {
        guard let container = AppGroup.containerURL else { return 0 }
        let fm = FileManager.default
        let wanted = Array(thoughts.compactMap(\.photo).filter(isPhotoPath).prefix(mirroredPhotos))
        var copied = 0
        for rel in wanted {
            let dst = container.appendingPathComponent(rel, isDirectory: false)
            if fm.fileExists(atPath: dst.path) { continue }
            let src = notesDir.appendingPathComponent(rel, isDirectory: false)
            var isDir: ObjCBool = false
            guard fm.fileExists(atPath: src.path, isDirectory: &isDir), !isDir.boolValue else { continue }
            do {
                try fm.createDirectory(at: dst.deletingLastPathComponent(), withIntermediateDirectories: true)
                try fm.copyItem(at: src, to: dst)
                copied += 1
            } catch {
                continue
            }
        }
        pruneMirroredPhotos(keeping: Set(wanted))
        return copied
    }

    /// Deletes mirrored files nothing references, and the day folders they
    /// leave empty. Only ever touches `photos/<day>/<file>.jpg` inside the
    /// container; anything else in there is left alone.
    private static func pruneMirroredPhotos(keeping wanted: Set<String>) {
        guard let root = AppGroup.photosURL else { return }
        let fm = FileManager.default
        guard let days = try? fm.contentsOfDirectory(at: root, includingPropertiesForKeys: [.isDirectoryKey])
        else { return }
        for day in days {
            guard (try? day.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true else { continue }
            let files = (try? fm.contentsOfDirectory(at: day, includingPropertiesForKeys: nil)) ?? []
            for file in files {
                let rel = "photos/\(day.lastPathComponent)/\(file.lastPathComponent)"
                guard isPhotoPath(rel) else { continue }
                if !wanted.contains(rel) { try? fm.removeItem(at: file) }
            }
            if ((try? fm.contentsOfDirectory(at: day, includingPropertiesForKeys: nil)) ?? []).isEmpty {
                try? fm.removeItem(at: day)
            }
        }
    }

    /// Atomically writes the snapshot into the group container.
    /// Everything in the snapshot except `updatedAt`, as bytes, so "did anything
    /// change?" covers every field without listing them.
    ///
    /// The mirror used to compare four fields by hand. Three fields were added
    /// for conversations, spoken stars and room notes, and none of them reached
    /// the guard: a recording was read, held in memory for the diary window, and
    /// never written to the App Group, so the widget's Notes tab stayed empty
    /// while the file sat on disk (2026-09-11).
    static func contentKey(_ snapshot: NotesSnapshot) -> Data? {
        var copy = snapshot
        copy.updatedAt = Date(timeIntervalSince1970: 0)
        return try? encoder.encode(copy)
    }

    static func save(_ snapshot: NotesSnapshot) throws {
        guard let url = AppGroup.notesFileURL else {
            throw CocoaError(.fileNoSuchFile, userInfo: [NSLocalizedDescriptionKey: "App Group container \(AppGroup.id) unavailable"])
        }
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try encoder.encode(snapshot).write(to: url, options: .atomic)
    }

    static func highlightsURL(notesDir: URL) -> URL { notesDir.appendingPathComponent("highlights.md", isDirectory: false) }

    /// `★ claim` / `- ★ claim` lines of highlights.md, file order.
    static func readHighlights(notesDir: URL) -> [String] {
        guard let body = try? String(contentsOf: highlightsURL(notesDir: notesDir), encoding: .utf8) else { return [] }
        var out: [String] = []
        for raw in body.split(separator: "\n", omittingEmptySubsequences: true) {
            var t = raw.trimmingCharacters(in: .whitespaces)
            if t.hasPrefix("-") { t.removeFirst(); t = t.trimmingCharacters(in: .whitespaces) }
            guard t.hasPrefix("★") else { continue }
            t.removeFirst()
            t = t.trimmingCharacters(in: .whitespaces)
            if !t.isEmpty { out.append(t) }
        }
        return out
    }

    /// The human stars a thought: append-only, never pruned, dated. eve reads it, never writes it.
    static func star(_ claim: String, notesDir: URL, date: Date = .now) throws {
        let url = highlightsURL(notesDir: notesDir)
        let f = DateFormatter(); f.dateFormat = "yyyy-MM-dd HH:mm"
        var body = (try? String(contentsOf: url, encoding: .utf8)) ?? "# never forget\n\nStarred by hand from the diary window. Append-only.\n\n"
        if !body.hasSuffix("\n") { body += "\n" }
        body += "- ★ \(claim.trimmingCharacters(in: .whitespacesAndNewlines)) — starred \(f.string(from: date))\n"
        try body.write(to: url, atomically: true, encoding: .utf8)
    }

    // MARK: - The spoken half: eve's memory folder

    /// Where eve keeps what was SAID: its memory folder, separate from the notes
    /// directory. `EVE_MEMORY_DIR` overrides it, as it does for the daemon
    /// (notes_widget.py `memory_dir`). Only ever read from here: the daemon's
    /// one-time move of the old store skips a target that already exists, so a
    /// folder made by the widget would strand the owner's records.
    static func defaultMemoryDir() -> URL {
        if let override = ProcessInfo.processInfo.environment["EVE_MEMORY_DIR"], !override.isEmpty {
            return URL(fileURLWithPath: (override as NSString).expandingTildeInPath, isDirectory: true)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config/eve-bridge/memory", isDirectory: true)
    }

    /// `<memory>/records`: the records, `starred.md` and the dream journal.
    static func recordsDir(store: URL) -> URL { store.appendingPathComponent("records", isDirectory: true) }

    /// `<memory>/records/days`: one journal page per day eve talked, written the night after.
    static func journalDir(store: URL) -> URL {
        recordsDir(store: store).appendingPathComponent("days", isDirectory: true)
    }

    /// What a journal line shows where a diary line shows its clock time.
    static let noTime = "--:--"
    /// What forget leaves in a journal in place of a line. Never shown.
    static let forgotten = "(forgotten)"

    /// One journal page as the lines worth showing: its title (what the day was
    /// about) and at most two debts of eve's own, because a debt is the thing
    /// eve has not done yet. A line forget redacted is dropped. Mirrors
    /// notes_widget.py `parse_conversation`.
    static func parseConversation(_ body: String, date: String) -> Conversation? {
        var title = ""
        var owes: [String] = []
        for raw in body.split(whereSeparator: \.isNewline) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if title.isEmpty, line.hasPrefix("# ") {
                title = String(line.dropFirst(2)).trimmingCharacters(in: .whitespaces)
            } else if line.hasPrefix("- ") {
                let item = String(line.dropFirst(2))
                if item.lowercased().hasPrefix("eve owes") {
                    owes.append(item.trimmingCharacters(in: .whitespaces))
                }
            }
        }
        if title.contains(forgotten) { title = "" }
        owes = owes.prefix(2).filter { !$0.contains(forgotten) }
        guard !title.isEmpty || !owes.isEmpty else { return nil }
        return Conversation(date: date, time: noTime, title: title.isEmpty ? "A conversation" : title, owes: owes)
    }

    /// What was said, from the dream journals of today and yesterday (calendar
    /// days, local time), newest day first. The same two pages the daemon's
    /// desktop widget shows (notes_widget.py `collect_conversations`).
    static func readConversations(store: URL, today: Date = .now) -> [Conversation] {
        let cal = Calendar.current
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.calendar = cal
        f.timeZone = .current
        f.dateFormat = "yyyy-MM-dd"
        let days = [today, cal.date(byAdding: .day, value: -1, to: today)].compactMap { $0 }
        var out: [Conversation] = []
        for day in days {
            let date = f.string(from: day)
            let file = journalDir(store: store).appendingPathComponent("\(date).md", isDirectory: false)
            guard let body = try? String(contentsOf: file, encoding: .utf8),
                  let c = parseConversation(body, date: date) else { continue }
            out.append(c)
        }
        return out
    }

    /// Recordings of the room, newest first, read from `<memory>/transcripts/meetings/<day>/`
    /// (notes.py `notes_dir`: meeting notes live beside the transcripts).
    ///
    /// Only the head of each file is parsed: these carry a whole transcript, and
    /// the widget lists them rather than showing them. The file itself is what
    /// the owner opens or exports.
    static func readRoomNotes(store: URL, limit: Int = 40) -> [RoomNote] {
        guard limit > 0 else { return [] }
        let fm = FileManager.default
        let root = store.appendingPathComponent("transcripts/meetings", isDirectory: true)
        let days = ((try? fm.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)) ?? [])
            .filter { $0.hasDirectoryPath }
            .sorted { $0.lastPathComponent > $1.lastPathComponent }
        var out: [RoomNote] = []
        for day in days {
            let files = ((try? fm.contentsOfDirectory(at: day, includingPropertiesForKeys: nil)) ?? [])
                .filter { $0.pathExtension == "md" }
                .sorted { $0.lastPathComponent > $1.lastPathComponent }
            for file in files {
                guard let body = readRoomNoteHead(file) else { continue }
                var title = "", gist = "", words = 0
                var afterTitle = false
                for raw in body.split(separator: "\n", omittingEmptySubsequences: false).prefix(40) {
                    let line = raw.trimmingCharacters(in: .whitespaces)
                    if line.hasPrefix("words:") {
                        words = Int(line.dropFirst(6).trimmingCharacters(in: .whitespaces)) ?? 0
                    } else if title.isEmpty, line.hasPrefix("# ") {
                        title = String(line.dropFirst(2)).trimmingCharacters(in: .whitespaces)
                        afterTitle = true
                    } else if afterTitle, gist.isEmpty, !line.isEmpty, !line.hasPrefix("#"),
                              !line.hasPrefix("<!--"), !line.hasPrefix(">") {
                        gist = line
                    }
                }
                let stem = file.deletingPathExtension().lastPathComponent
                let digits = stem.prefix(4)
                let time = digits.count == 4 && digits.allSatisfy(\.isNumber)
                    ? "\(digits.prefix(2)):\(digits.suffix(2))" : "--:--"
                out.append(RoomNote(date: day.lastPathComponent, time: String(time),
                                    title: title.isEmpty ? "Notes" : title, gist: gist,
                                    words: words, path: file.path))
                if out.count >= limit { return out }
            }
        }
        return out
    }

    /// Read only the header the room-note list displays, including lines split
    /// across read boundaries. A long transcript must not be loaded on each sync.
    static func readRoomNoteHead(_ file: URL, chunkBytes: Int = 8192) -> String? {
        guard chunkBytes > 0, let handle = try? FileHandle(forReadingFrom: file) else { return nil }
        defer { try? handle.close() }
        var head = Data()
        var lines = 0
        do {
            while let chunk = try handle.read(upToCount: chunkBytes), !chunk.isEmpty {
                for (i, byte) in chunk.enumerated() where byte == 0x0A {
                    lines += 1
                    if lines == 40 {
                        head.append(chunk.prefix(i + 1))
                        return String(data: head, encoding: .utf8)
                    }
                }
                head.append(chunk)
            }
        } catch { return nil }
        return String(data: head, encoding: .utf8)
    }

    /// Seek backwards until enough valid records are found. Decode whole lines
    /// so a UTF-8 character or JSON record spanning chunks remains intact.
    static func readThoughts(_ file: URL, limit: Int = limit, chunkBytes: Int = 65536) -> [Thought] {
        guard limit > 0, chunkBytes > 0, let handle = try? FileHandle(forReadingFrom: file) else { return [] }
        defer { try? handle.close() }
        let decoder = JSONDecoder()
        var thoughts: [Thought] = []
        var suffix = Data()
        func append(_ line: Data.SubSequence) {
            if !line.isEmpty, let thought = try? decoder.decode(Thought.self, from: Data(line)) {
                thoughts.append(thought)
            }
        }
        do {
            var offset = try handle.seekToEnd()
            while offset > 0 {
                let length = min(UInt64(chunkBytes), offset)
                offset -= length
                try handle.seek(toOffset: offset)
                guard var chunk = try handle.read(upToCount: Int(length)), chunk.count == Int(length) else { return [] }
                chunk.append(suffix)
                var end = chunk.endIndex
                while let newline = chunk[..<end].lastIndex(of: 0x0A) {
                    append(chunk[chunk.index(after: newline)..<end])
                    if thoughts.count >= limit { return thoughts }
                    end = newline
                }
                suffix = Data(chunk[..<end])
            }
            append(suffix[...])
        } catch { return [] }
        return thoughts
    }

    /// `- claim (YYYY-MM-DD)` lines, an optional ★ after the bullet (records.py `_STAR_LINE`).
    private static let starLine = try! NSRegularExpression(pattern: #"^\s*[-*]\s+(?:★\s*)?(.+?)\s*$"#)
    /// The trailing ` (YYYY-MM-DD)` a star is dated with (records.py `_STAR_DATE`).
    private static let starDate = try! NSRegularExpression(pattern: #"^(.*?)\s*\((\d{4}-\d{2}-\d{2})\)$"#)
    /// A leading YAML frontmatter block, which is never a star (records.py `_FRONTMATTER`).
    private static let frontmatter = try! NSRegularExpression(pattern: #"\A---\n.*?\n---[ \t]*(?:\n|\z)"#,
                                                              options: [.dotMatchesLineSeparators])

    /// What the owner asked eve to remember, from `<memory>/records/starred.md`:
    /// text only, the date dropped, oldest first, the newest `limit` of them.
    /// Mirrors records.py `stars` (and its default limit), so the owner sees the
    /// same claims eve starts every conversation with.
    static func readSpokenStars(store: URL, limit: Int = 40) -> [String] {
        let url = recordsDir(store: store).appendingPathComponent("starred.md", isDirectory: false)
        guard limit > 0, let raw = try? String(contentsOf: url, encoding: .utf8) else { return [] }
        let body = frontmatter.stringByReplacingMatches(in: raw, range: NSRange(raw.startIndex..., in: raw),
                                                        withTemplate: "")
        var out: [String] = []
        for sub in body.split(whereSeparator: \.isNewline) {
            let line = String(sub)
            guard let m = starLine.firstMatch(in: line, range: NSRange(line.startIndex..., in: line)),
                  let r = Range(m.range(at: 1), in: line) else { continue }
            var claim = String(line[r])
            if let d = starDate.firstMatch(in: claim, range: NSRange(claim.startIndex..., in: claim)),
               let t = Range(d.range(at: 1), in: claim) {
                claim = claim[t].trimmingCharacters(in: .whitespaces)
            }
            if !claim.isEmpty { out.append(claim) }
        }
        return Array(out.suffix(limit))
    }

    /// Reads the daemon's notes directory: diary lines (newest first), memory records
    /// (newest first), the profile and the reflections. Returns the snapshot plus the
    /// newest day file (the one worth watching for appends).
    static func read(notesDir: URL, store: URL? = nil) -> (snapshot: NotesSnapshot, newestFile: URL?) {
        let fm = FileManager.default
        let files = ((try? fm.contentsOfDirectory(at: notesDir, includingPropertiesForKeys: nil)) ?? [])
            .filter { $0.pathExtension == "md" && $0.lastPathComponent != "profile.md" }
            .sorted { $0.lastPathComponent > $1.lastPathComponent }

        var notes: [Note] = []
        var reflections: [Reflection] = []
        for file in files {
            let date = file.deletingPathExtension().lastPathComponent
            guard let body = try? String(contentsOf: file, encoding: .utf8) else { continue }
            if notes.count < limit {
                for line in body.split(separator: "\n", omittingEmptySubsequences: true).reversed() {
                    if let note = NoteLine.parse(String(line), date: date) {
                        notes.append(note)
                        if notes.count >= limit { break }
                    }
                }
            }
            let insights = NoteLine.reflections(in: body)
            if !insights.isEmpty, reflections.count < 30 {
                reflections.append(Reflection(date: date, insights: insights))
            }
        }

        let memory = notesDir.appendingPathComponent("memory.jsonl", isDirectory: false)
        let thoughts = readThoughts(memory)
        let profileURL = notesDir.appendingPathComponent("profile.md", isDirectory: false)
        let profile = (try? String(contentsOf: profileURL, encoding: .utf8)) ?? ""
        let highlights = readHighlights(notesDir: notesDir)

        let memoryDir = store ?? defaultMemoryDir()
        let snap = NotesSnapshot(updatedAt: .now, notesDir: notesDir.path, notes: notes, thoughts: thoughts,
                                 profile: profile, reflections: reflections, highlights: highlights,
                                 conversations: readConversations(store: memoryDir),
                                 spokenStars: readSpokenStars(store: memoryDir),
                                 roomNotes: readRoomNotes(store: memoryDir))
        return (snap, files.first)
    }
}


// Learning is deliberately separate from the room diary and its automatic memory.
struct LearningLesson: Codable, Identifiable, Sendable {
    let id: String
    let topic: String
    let problem: String
    let stage: String
    let updated: Double
    let demo: Bool
}

struct LearningSnapshot: Codable, Sendable {
    let total: Int
    let completed: Int
    let lessons: [LearningLesson]
    static let empty = LearningSnapshot(total: 0, completed: 0, lessons: [])

    static var fileURL: URL? {
        AppGroup.containerURL?.appendingPathComponent("learning-dashboard.json")
    }

    static func load() -> LearningSnapshot {
        guard let url = fileURL, let data = try? Data(contentsOf: url),
              let value = try? JSONDecoder().decode(LearningSnapshot.self, from: data) else { return .empty }
        return value
    }

    /// The helper app can read the bridge store; the widget only gets this summary.
    static func mirror() throws -> Bool {
        let path = ProcessInfo.processInfo.environment["EVE_LEARNING_DIR"] ?? "~/.config/eve-bridge/learning"
        let directory = URL(fileURLWithPath: (path as NSString).expandingTildeInPath)
        let source = directory.appendingPathComponent("dashboard.json")
        guard FileManager.default.fileExists(atPath: source.path), let destination = fileURL else { return false }
        let data = try Data(contentsOf: source)
        _ = try JSONDecoder().decode(LearningSnapshot.self, from: data)
        if (try? Data(contentsOf: destination)) == data { return false }
        try data.write(to: destination, options: .atomic)
        return true
    }
}
