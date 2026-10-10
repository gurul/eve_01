// The diary window: what the widget shows, in full — every thought with the
// observations, what changed, tags, novelty and importance behind it; a
// feelings timeline; eve's profile of the room and its human; the evening
// reflections. Opened by tapping the widget (stackchan://diary) or from the
// menu-bar item.
//
// Drawn in the Eve Show-and-Tell look (Shared/EveBrand.swift): always paper,
// ink text, cut-paper cards. The feelings chart points keep the robot's LED
// colours (`Color.moodColor`), because the caption says so.

import Charts
import SwiftUI

struct DiaryView: View {
    @State private var mirror = NotesMirror.shared
    @State private var tab: Tab = .thoughts

    enum Tab: String, CaseIterable, Identifiable {
        case talking = "Talking", recordings = "Notes", thoughts = "Thoughts", photos = "Photos",
             feelings = "Feelings", profile = "Profile", reflections = "Dreams"
        var id: String { rawValue }
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Rectangle().fill(Color.brandInk).frame(height: 2)
            Group {
                switch tab {
                case .talking: TalkingList(conversations: mirror.snapshot.conversations,
                                           stars: mirror.snapshot.spokenStars)
                case .recordings: RoomNotesList(notes: mirror.snapshot.roomNotes)
                case .thoughts: ThoughtsList(thoughts: mirror.snapshot.thoughts, notes: mirror.snapshot.notes)
                case .photos: PhotosGrid(thoughts: mirror.snapshot.thoughts, notesDir: mirror.notesDir)
                case .feelings: FeelingsView(thoughts: mirror.snapshot.thoughts)
                case .profile: ProfileView(profile: mirror.snapshot.profile, highlights: mirror.snapshot.highlights)
                case .reflections: ReflectionsView(reflections: mirror.snapshot.reflections)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .frame(minWidth: 560, minHeight: 480)
        .background(Color.brandPaper)
        .foregroundStyle(Color.brandInk)
        .tint(Color.brandInk)
        .environment(\.colorScheme, .light)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                let mood = mirror.snapshot.mood
                RobotFace(size: 44, compact: true)
                VStack(alignment: .leading, spacing: 1) {
                    InkHeading("eve's diary", size: 24)
                    Group {
                        if let mood {
                            Text("feeling \(MoodColor.emoji(for: mood.label)) \(mood.label) · valence \(mood.valence) · arousal \(mood.arousal) · \(mood.time)")
                        } else {
                            Text("no feelings recorded yet")
                        }
                    }
                    .font(BrandFont.body(12))
                    .foregroundStyle(Color.brandInkSoft)
                    .lineLimit(1)
                }
                Spacer()
                Button { mirror.sync() } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                    .buttonStyle(ChunkyButtonStyle())
                    .help("Refresh now")
            }
            // Paper tabs in place of a segmented control, on their own row so they
            // fit at the minimum width; they scroll sideways only if they ever have to.
            ViewThatFits(in: .horizontal) {
                tabRow
                ScrollView(.horizontal, showsIndicators: false) { tabRow }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    private var tabRow: some View {
        HStack(spacing: 8) {
            ForEach(Tab.allCases) { t in
                Button(t.rawValue) { tab = t }
                    .buttonStyle(TabPillStyle(selected: tab == t))
                    .accessibilityAddTraits(tab == t ? .isSelected : [])
            }
        }
        // Room for the selected tab's offset shadow.
        .padding(.leading, 1)
        .padding(.top, 2)
        .padding(.trailing, 4)
        .padding(.bottom, 4)
    }
}

extension Color {
    /// The robot's LED colour for a feeling (mirrors the firmware, see `MoodColor.rgb`).
    static func moodColor(_ t: Thought?) -> Color {
        guard let t else { return .brandInkSoft }
        let (r, g, b) = MoodColor.rgb(valence: t.valence, arousal: t.arousal, label: t.label)
        return Color(red: r, green: g, blue: b)
    }
}

/// A handwritten section kicker, such as a day heading.
private struct Kicker: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(BrandFont.hand(16))
            .foregroundStyle(Color.brandInkSoft)
    }
}

/// A sun star with an ink outline: a claim eve keeps for good.
private struct StarGlyph: View {
    var filled = true
    var size: CGFloat = 12

    var body: some View {
        ZStack {
            if filled {
                Image(systemName: "star.fill").foregroundStyle(Color.brandSun)
                Image(systemName: "star").foregroundStyle(Color.brandInk)
            } else {
                Image(systemName: "star").foregroundStyle(Color.brandInkSoft)
            }
        }
        .font(.system(size: size, weight: .semibold))
    }
}

/// An empty tab: the robot, a heading and one friendly line.
private struct EmptyState: View {
    let title: String
    let message: String
    var line: String? = "hmm..."

    var body: some View {
        VStack(spacing: 10) {
            Spacer()
            RobotFace(size: 96, line: line)
            Text(title)
                .font(BrandFont.display(20, black: false))
                .foregroundStyle(Color.brandInk)
            Text(message)
                .font(BrandFont.body(13))
                .foregroundStyle(Color.brandInkSoft)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
}

// MARK: - Notes eve took of the room
//
// A recording is a file, not a card: it holds a whole transcript. So this lists
// them and gets out of the way, with the two things you actually want — open it,
// or save a copy somewhere of your own.

private struct RoomNotesList: View {
    let notes: [RoomNote]
    @State private var saving: String?

    private var days: [(String, [RoomNote])] {
        var out: [(String, [RoomNote])] = []
        for n in notes {
            if let last = out.last, last.0 == n.date { out[out.count - 1].1.append(n) } else { out.append((n.date, [n])) }
        }
        return out
    }

    var body: some View {
        if notes.isEmpty {
            EmptyState(title: "No recordings yet",
                       message: "Say “start taking notes” and eve writes down what is said in the room until you tell it to stop, tap it, or run `eve-bridge take-notes stop`.")
        } else {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    ForEach(days, id: \.0) { day, items in
                        VStack(alignment: .leading, spacing: 12) {
                            Kicker(day)
                            ForEach(items) { note in
                                VStack(alignment: .leading, spacing: 5) {
                                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                                        Text(note.time).font(BrandFont.body(11, bold: true)).monospacedDigit()
                                            .foregroundStyle(Color.brandInkSoft)
                                        Text(note.title).font(BrandFont.display(15, black: false))
                                        Spacer(minLength: 8)
                                        if note.words > 0 {
                                            Text("\(note.words) words").font(BrandFont.body(11))
                                                .foregroundStyle(Color.brandInkSoft)
                                        }
                                    }
                                    if !note.gist.isEmpty {
                                        Text(note.gist).font(BrandFont.body(13)).foregroundStyle(Color.brandInkSoft)
                                            .fixedSize(horizontal: false, vertical: true)
                                    }
                                    HStack(spacing: 10) {
                                        Button("Open") { open(note) }
                                        Button("Save a copy…") { save(note) }
                                        Button("Show in Finder") { reveal(note) }
                                    }
                                    .buttonStyle(.link)
                                    .tint(Color.brandInk)
                                    .font(BrandFont.body(12, bold: true))
                                }
                                .padding(12)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .paperCard(shadow: .brandSky)
                            }
                        }
                    }
                }
                .padding(16)
                .padding(.trailing, 4)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func open(_ note: RoomNote) {
        NSWorkspace.shared.open(URL(fileURLWithPath: note.path))
    }

    private func reveal(_ note: RoomNote) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: note.path)])
    }

    /// Save a copy wherever the owner wants it. The original stays where eve
    /// put it — this is a download, not a move, so nothing that has been indexed
    /// or linked goes missing.
    private func save(_ note: RoomNote) {
        let src = URL(fileURLWithPath: note.path)
        let panel = NSSavePanel()
        panel.nameFieldStringValue = src.lastPathComponent
        panel.allowedContentTypes = [.init(filenameExtension: "md") ?? .plainText]
        panel.canCreateDirectories = true
        panel.title = "Save these notes"
        panel.begin { response in
            guard response == .OK, let dst = panel.url else { return }
            do {
                if FileManager.default.fileExists(atPath: dst.path) {
                    try FileManager.default.removeItem(at: dst)
                }
                try FileManager.default.copyItem(at: src, to: dst)
            } catch {
                NSSound.beep()
            }
        }
    }
}

// MARK: - Talking
//
// What eve heard, kept apart from what eve saw. The starred claims sit at the
// top because they are permanent and the owner put them there by voice; the
// conversations follow, newest first, each showing what it was about and any debt
// of eve's own. A debt is the line the owner most wants to see: it is the thing
// eve has not done yet.

private struct TalkingList: View {
    let conversations: [Conversation]
    let stars: [String]

    private var days: [(String, [Conversation])] {
        var out: [(String, [Conversation])] = []
        for c in conversations {
            if let last = out.last, last.0 == c.date { out[out.count - 1].1.append(c) } else { out.append((c.date, [c])) }
        }
        return out
    }

    var body: some View {
        if conversations.isEmpty && stars.isEmpty {
            EmptyState(title: "Nothing said yet",
                       message: "Say “hey eve” to talk to it. Say “remember that” and it keeps what you just said, for good.",
                       line: "Hey there!")
        } else {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if !stars.isEmpty {
                        VStack(alignment: .leading, spacing: 6) {
                            Kicker("Remembered for good")
                            ForEach(Array(stars.reversed().enumerated()), id: \.offset) { _, claim in
                                HStack(alignment: .firstTextBaseline, spacing: 8) {
                                    StarGlyph()
                                    Text(claim).font(BrandFont.body(13))
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }
                        }
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .paperCard(shadow: .brandPink, fill: .brandSunWash)
                    }
                    ForEach(days, id: \.0) { day, items in
                        VStack(alignment: .leading, spacing: 10) {
                            Kicker(day)
                            ForEach(items) { c in
                                VStack(alignment: .leading, spacing: 4) {
                                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                                        Text(c.time).font(BrandFont.body(11, bold: true)).monospacedDigit()
                                            .foregroundStyle(Color.brandInkSoft)
                                        Text(c.title).font(BrandFont.display(15, black: false))
                                    }
                                    ForEach(Array(c.owes.enumerated()), id: \.offset) { _, owed in
                                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                                            Image(systemName: "arrow.uturn.backward")
                                                .font(.system(size: 10, weight: .bold)).foregroundStyle(Color.brandPink)
                                            Text(owed).font(BrandFont.body(13)).foregroundStyle(Color.brandInk)
                                                .fixedSize(horizontal: false, vertical: true)
                                        }
                                        .padding(.leading, 2)
                                    }
                                }
                                .padding(12)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .paperCard(shadow: c.owes.isEmpty ? .brandSky : .brandPink)
                            }
                        }
                    }
                }
                .padding(16)
                .padding(.trailing, 4)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}

// MARK: - Thoughts

private struct ThoughtsList: View {
    let thoughts: [Thought]
    let notes: [Note]
    @State private var showUnwritten = false

    private var visible: [Thought] { showUnwritten ? thoughts : thoughts.filter(\.written) }

    private var days: [(String, [Thought])] {
        var out: [(String, [Thought])] = []
        for t in visible {
            if let last = out.last, last.0 == t.day { out[out.count - 1].1.append(t) } else { out.append((t.day, [t])) }
        }
        return out
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Toggle("Show unwritten candidates (kept in memory only)", isOn: $showUnwritten)
                    .toggleStyle(.checkbox).font(BrandFont.body(12))
                Spacer()
                Text("\(visible.count) of \(thoughts.count)").font(BrandFont.body(12))
                    .foregroundStyle(Color.brandInkSoft)
            }
            .padding(.horizontal, 16).padding(.vertical, 8)
            if thoughts.isEmpty {
                EmptyDiary(notes: notes)
            } else {
                List {
                    ForEach(days, id: \.0) { day, items in
                        Section(header: Kicker(dayLabel(day))) {
                            ForEach(items) {
                                ThoughtCard(thought: $0)
                                    .listRowSeparator(.hidden)
                                    .listRowBackground(Color.clear)
                            }
                        }
                    }
                }
                .listStyle(.inset)
                .scrollContentBackground(.hidden)
                .background(Color.brandPaper)
            }
        }
    }

    private func dayLabel(_ day: String) -> String {
        let f = DateFormatter(); f.dateFormat = "yyyy-MM-dd"
        guard let d = f.date(from: day) else { return day }
        if Calendar.current.isDateInToday(d) { return "Today · \(day)" }
        if Calendar.current.isDateInYesterday(d) { return "Yesterday · \(day)" }
        return d.formatted(date: .complete, time: .omitted)
    }
}

private struct ThoughtCard: View {
    let thought: Thought
    @State private var open = false
    @State private var mirror = NotesMirror.shared
    private var starred: Bool { mirror.snapshot.highlights.contains { $0.hasPrefix(thought.thought) } }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(thought.time).font(BrandFont.body(11, bold: true)).monospacedDigit()
                    .foregroundStyle(Color.brandInkSoft)
                BrandPill("\(MoodColor.emoji(for: thought.label)) \(thought.label)", fill: BrandMood.color(thought.label))
                Text("novelty \(thought.novelty) · importance \(thought.importance)")
                    .font(BrandFont.body(11)).foregroundStyle(Color.brandInkSoft)
                if !thought.written {
                    BrandPill("unwritten", fill: .brandDesk, size: 9.5)
                }
                Spacer()
                Text("yaw \(thought.yaw >= 0 ? "+" : "")\(thought.yaw)  pitch \(thought.pitch)")
                    .font(BrandFont.body(11)).monospacedDigit().foregroundStyle(Color.brandInkSoft.opacity(0.75))
                Button {
                    if !starred { mirror.star(thought.thought) }
                } label: {
                    StarGlyph(filled: starred, size: 13)
                }
                .buttonStyle(.plain)
                .help(starred ? "Starred — eve will never forget this" : "Star: eve must never forget this")
            }
            Text(thought.thought).font(BrandFont.body(14)).foregroundStyle(Color.brandInk)
                .fixedSize(horizontal: false, vertical: true)
            if let photo = PhotoFile.url(thought.photo, notesDir: mirror.notesDir) {
                PhotoThumb(url: photo, height: open ? 220 : 96)
                    .help("eve thought this was worth a picture — click the card for the full size")
            }
            if open {
                Details(thought: thought)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .paperCard(shadow: BrandMood.color(thought.label))
        // Room for the offset shadow inside the list row.
        .padding(.trailing, 6)
        .padding(.bottom, 6)
        .padding(.top, 2)
        .contentShape(Rectangle())
        .onTapGesture { withAnimation(.easeInOut(duration: 0.15)) { open.toggle() } }
        .help(open ? "Click to collapse" : "Click for observations, changes and tags")
    }
}

private struct Details: View {
    let thought: Thought

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            if !thought.observations.isEmpty {
                Labeled("saw") { ForEach(thought.observations, id: \.self) { Text("• \($0)") } }
            }
            if !thought.changed.isEmpty {
                Labeled("changed") { ForEach(thought.changed, id: \.self) { Text("• \($0)") } }
            }
            if !thought.tags.isEmpty {
                HStack(spacing: 6) {
                    ForEach(thought.tags, id: \.self) { tag in
                        BrandPill(tag, fill: .brandPinkSoft, size: 9.5)
                    }
                }
            }
            HStack(spacing: 12) {
                Gauge(value: Double(thought.valence + 100), in: 0...200) { Text("valence") }
                    .tint(Color.moodColor(thought))
                Gauge(value: Double(thought.arousal + 100), in: 0...200) { Text("arousal") }
                    .tint(Color.moodColor(thought))
            }
            .gaugeStyle(.accessoryLinearCapacity)
            .font(BrandFont.body(11))
            .frame(maxWidth: 360)
        }
        .font(BrandFont.body(13))
        .padding(.top, 2)
    }
}

private struct Labeled<Content: View>: View {
    let label: String
    @ViewBuilder let content: Content

    init(_ label: String, @ViewBuilder content: () -> Content) { self.label = label; self.content = content() }

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Text(label).font(BrandFont.body(11, bold: true)).foregroundStyle(Color.brandInkSoft)
                .frame(width: 56, alignment: .trailing)
            VStack(alignment: .leading, spacing: 2) { content }
        }
    }
}

private struct EmptyDiary: View {
    let notes: [Note]

    var body: some View {
        EmptyState(title: notes.isEmpty ? "Nothing noticed yet." : "The memory file has not been written yet.",
                   message: notes.isEmpty ? "eve explores the room once Claude has been idle for ten minutes, and writes what it finds interesting."
                   : "Older notes appear in the widget; the diary fills in as new thoughts arrive.")
    }
}

// MARK: - Feelings

private struct FeelingsView: View {
    let thoughts: [Thought]

    private var points: [Thought] { Array(thoughts.prefix(120).reversed()) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if points.count < 2 {
                Spacer()
                Text("A feelings timeline appears after a few thoughts.")
                    .font(BrandFont.body(14)).foregroundStyle(Color.brandInkSoft)
                    .frame(maxWidth: .infinity)
                Spacer()
            } else {
                Text("Valence (how good) and arousal (how keen), per thought. Colours match the robot's LEDs.")
                    .font(BrandFont.body(12)).foregroundStyle(Color.brandInkSoft)
                Chart {
                    ForEach(points) { t in
                        LineMark(x: .value("time", t.date), y: .value("valence", t.valence), series: .value("s", "valence"))
                            .foregroundStyle(Color.brandTeal)
                            .interpolationMethod(.monotone)
                        LineMark(x: .value("time", t.date), y: .value("arousal", t.arousal), series: .value("s", "arousal"))
                            .foregroundStyle(Color.brandPink)
                            .interpolationMethod(.monotone)
                        PointMark(x: .value("time", t.date), y: .value("valence", t.valence))
                            .foregroundStyle(Color.moodColor(t))
                            .symbolSize(40)
                    }
                    RuleMark(y: .value("zero", 0)).foregroundStyle(Color.brandInkSoft.opacity(0.3))
                }
                .chartYScale(domain: -100...100)
                .chartLegend(.hidden)
                .frame(minHeight: 220)
                .padding(12)
                .paperCard(shadow: .brandSky)
                .padding(.trailing, 4)
                // Colour only the dot: teal or pink words on paper would be too faint to read.
                HStack(spacing: 16) {
                    Label { Text("valence").foregroundStyle(Color.brandInk) } icon: {
                        Image(systemName: "circle.fill").foregroundStyle(Color.brandTeal)
                    }
                    Label { Text("arousal").foregroundStyle(Color.brandInk) } icon: {
                        Image(systemName: "circle.fill").foregroundStyle(Color.brandPink)
                    }
                }
                .font(BrandFont.body(12, bold: true))
                MoodTally(thoughts: thoughts)
            }
        }
        .padding(16)
    }
}

private struct MoodTally: View {
    let thoughts: [Thought]

    private var counts: [(String, Int)] {
        var d: [String: Int] = [:]
        for t in thoughts { d[t.label, default: 0] += 1 }
        return d.sorted { $0.value > $1.value }
    }

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 10) {
                ForEach(counts, id: \.0) { label, n in
                    BrandPill("\(MoodColor.emoji(for: label)) \(label) \(n)", fill: BrandMood.color(label), size: 11)
                }
            }
            .padding(.vertical, 1)
        }
    }
}

// MARK: - Profile

private struct ProfileView: View {
    let profile: String
    let highlights: [String]

    private var blocks: [(String, String)] {
        var out: [(String, String)] = []
        for chunk in profile.components(separatedBy: "\n## ") {
            var text = chunk
            if text.hasPrefix("## ") { text.removeFirst(3) }
            guard let nl = text.firstIndex(of: "\n") else { continue }
            out.append((String(text[..<nl]).trimmingCharacters(in: .whitespaces), String(text[nl...]).trimmingCharacters(in: .whitespacesAndNewlines)))
        }
        return out
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 6) {
                        StarGlyph(size: 15)
                        Text("Never forget").font(BrandFont.display(17, black: false))
                    }
                    if highlights.isEmpty {
                        Text("Nothing starred yet. Press ★ on a thought to make it something eve always keeps in mind; eve proposes candidates in its dreams but never stars them itself.")
                            .font(BrandFont.body(13)).foregroundStyle(Color.brandInkSoft)
                            .fixedSize(horizontal: false, vertical: true)
                    } else {
                        ForEach(highlights.reversed(), id: \.self) {
                            Text("★ \($0)").font(BrandFont.body(14)).textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .paperCard(shadow: .brandPink, fill: .brandSunWash)
                if profile.isEmpty {
                    Text("No profile yet. eve writes one after its first evening reflection: what is in the room, what it has learned about you, its own open questions, and what it ignores.")
                        .font(BrandFont.body(13)).foregroundStyle(Color.brandInkSoft)
                        .fixedSize(horizontal: false, vertical: true)
                } else {
                    ForEach(blocks, id: \.0) { title, body in
                        VStack(alignment: .leading, spacing: 6) {
                            Text(title.capitalized).font(BrandFont.display(16, black: false))
                            Text(body).font(BrandFont.body(14)).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                        }
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .paperCard(shadow: .brandSky)
                    }
                }
            }
            .padding(16)
            .padding(.trailing, 4)
        }
    }
}

// MARK: - Reflections

private struct ReflectionsView: View {
    let reflections: [Reflection]

    var body: some View {
        if reflections.isEmpty {
            VStack {
                Spacer()
                Text("No dreams yet. Each evening (or after a busy day) eve sleeps on the day: what the notes add up to, what it should keep, and what it proposes to never forget (★ candidates — star them from the Thoughts tab, or here in the file).")
                    .font(BrandFont.body(14)).foregroundStyle(Color.brandInkSoft)
                    .multilineTextAlignment(.center).frame(maxWidth: 420)
                Spacer()
            }
            .frame(maxWidth: .infinity)
        } else {
            List(reflections) { r in
                Section(header: Kicker(r.date)) {
                    ForEach(r.insights, id: \.self) {
                        Text("• \($0)")
                            .font(BrandFont.body(14))
                            .fixedSize(horizontal: false, vertical: true)
                            .listRowBackground(Color.clear)
                    }
                }
            }
            .listStyle(.inset)
            .scrollContentBackground(.hidden)
            .background(Color.brandPaper)
        }
    }
}


// MARK: - Photos

/// Resolving a record's photo path. The helper app is not sandboxed, so it
/// reads the daemon's own file; the mirrored copy in the App Group container
/// is the fallback (and the only thing the widget extension can read).
enum PhotoFile {
    static func url(_ rel: String?, notesDir: URL) -> URL? {
        guard let rel, !rel.isEmpty else { return nil }
        let direct = notesDir.appendingPathComponent(rel, isDirectory: false)
        if FileManager.default.fileExists(atPath: direct.path) { return direct }
        return AppGroup.photo(rel)
    }
}

private struct PhotoThumb: View {
    let url: URL
    var height: CGFloat = 96

    var body: some View {
        if let image = NSImage(contentsOf: url) {
            Image(nsImage: image)
                .resizable()
                .aspectRatio(contentMode: .fit)
                .clipShape(RoundedRectangle(cornerRadius: 5))
                .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(Color.brandInk, lineWidth: 1.5))
                .frame(maxWidth: .infinity, maxHeight: height, alignment: .leading)
        }
    }
}

/// Everything eve photographed, newest first, with the thought it kept it for.
private struct PhotosGrid: View {
    let thoughts: [Thought]
    let notesDir: URL

    private var items: [(Thought, URL)] {
        thoughts.compactMap { t in PhotoFile.url(t.photo, notesDir: notesDir).map { (t, $0) } }
    }

    var body: some View {
        if items.isEmpty {
            EmptyState(title: "No photos yet",
                       message: "eve keeps a picture when a view surprises it — not for every thought.")
        } else {
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 240), spacing: 16)], spacing: 16) {
                    ForEach(items, id: \.0.id) { thought, url in
                        VStack(alignment: .leading, spacing: 6) {
                            PhotoThumb(url: url, height: 180)
                            HStack(spacing: 6) {
                                Text("\(thought.day) \(thought.time)")
                                    .font(BrandFont.body(11, bold: true)).monospacedDigit()
                                    .foregroundStyle(Color.brandInkSoft)
                                BrandPill("\(MoodColor.emoji(for: thought.label)) \(thought.label)",
                                          fill: BrandMood.color(thought.label), size: 9.5)
                                Spacer()
                                Text("novelty \(thought.novelty)")
                                    .font(BrandFont.body(11)).foregroundStyle(Color.brandInkSoft.opacity(0.75))
                            }
                            Text(thought.thought).font(BrandFont.body(13)).fixedSize(horizontal: false, vertical: true)
                        }
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .paperCard(shadow: .brandTeal)
                        .onTapGesture { NSWorkspace.shared.open(url) }
                        .help("Click to open the full picture")
                    }
                }
                .padding(16)
                .padding(.trailing, 4)
            }
        }
    }
}
