// WidgetKit extension — sandboxed, reads only the mirrored notes.json from the App Group.
//
// Design: the Eve Show-and-Tell look — a sheet of paper with ink text and
// cut-paper cards (see Shared/EveBrand.swift). In full colour it is always paper,
// on light and dark desktops alike. When the desktop draws it vibrant or accented
// (an app window in front), the system removes the paper, so text and lines use
// `BrandStyle`, which turns white and drops fills and shadows in those modes.
//
// The diary widget shows eve's name and current feeling, a debt of eve's own
// if it has one, and the newest thoughts as cards whose offset shadow is the
// colour of the feeling they were written in, in four sizes up to extra large.
// Tapping it opens the chat window in the helper app (stackchan://chat); the
// diary window is in the helper's menu. The learning widget shows the saved
// lessons and opens the learning dashboard (stackchan://learning).

import AppIntents
import SwiftUI
import WidgetKit

@main
struct StackChanNotesWidgetBundle: WidgetBundle {
    /// The brand fonts ship in this extension's Resources; register them for the
    /// extension process before any view asks for them.
    init() { BrandFonts.register() }

    var body: some Widget {
        StackChanNotesWidget()
        EveLearningWidget()
    }
}

struct NotesEntry: TimelineEntry {
    let date: Date
    let snapshot: NotesSnapshot
    /// eve's power switch as the card should draw it (EvePower.face).
    var power: EvePower.Face = .unknown
}

/// The card's power button. The extension is sandboxed, so `perform` only
/// leaves a request in the App Group; the menu-bar helper (PowerRelay) does the
/// launchctl work and writes the state back, which reloads the card.
struct SetEvePowerIntent: AppIntent {
    static let title: LocalizedStringResource = "Turn eve on or off"
    static let description = IntentDescription("Stops or starts the eve daemon on this Mac.")

    @Parameter(title: "On")
    var on: Bool

    init() {}
    init(on: Bool) { self.on = on }

    func perform() async throws -> some IntentResult {
        let request = try EvePower.writeRequest(on: on)
        // Stay until the helper has answered (about a second) or is plainly not there. The card is
        // redrawn when this returns, so it shows the result of the press rather than "stopping…".
        let deadline = Date.now.addingTimeInterval(EvePower.answerWait)
        while !EvePower.answered(request), Date.now < deadline {
            try? await Task.sleep(for: .milliseconds(200))
        }
        return .result()
    }
}

struct NotesProvider: TimelineProvider {
    func placeholder(in context: Context) -> NotesEntry {
        NotesEntry(date: .now, snapshot: .placeholder)
    }

    func getSnapshot(in context: Context, completion: @escaping (NotesEntry) -> Void) {
        completion(context.isPreview
                   ? NotesEntry(date: .now, snapshot: .placeholder, power: .on)
                   : NotesEntry(date: .now, snapshot: NoteStore.load(), power: EvePower.face()))
    }

    /// One entry, refreshed every 15 min as a fallback. The helper app's
    /// `WidgetCenter.reloadAllTimelines()` is the real trigger. While a power
    /// request is pending the card re-reads sooner, so a helper that never
    /// answers turns into "waiting" rather than "stopping…" forever.
    func getTimeline(in context: Context, completion: @escaping (Timeline<NotesEntry>) -> Void) {
        let power = EvePower.face()
        let entry = NotesEntry(date: .now, snapshot: NoteStore.load(), power: power)
        let pending = power == .turningOn || power == .turningOff
        let next: TimeInterval = pending ? EvePower.helperTimeout + 1 : 15 * 60
        completion(Timeline(entries: [entry], policy: .after(.now.addingTimeInterval(next))))
    }
}

struct StackChanNotesWidget: Widget {
    static let kind = "StackChanNotes"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: Self.kind, provider: NotesProvider()) { entry in
            NotesView(entry: entry)
                .containerBackground(for: .widget) { NotesCardBackground() }
                .widgetURL(AppGroup.chatURL)
        }
        .configurationDisplayName("eve's diary")
        .description("What eve noticed around your desk, and how it felt. Tap to chat with eve.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge, .systemExtraLarge])
    }
}

/// The paper the diary widget is drawn on.
struct NotesCardBackground: View {
    var body: some View { Color.brandPaper }
}

extension Color {
    /// The brand colour for the feeling a thought was written in.
    static func mood(_ t: Thought?) -> Color {
        BrandMood.color(t?.label)
    }
}

private let learningURL = URL(string: "stackchan://learning")!

struct NotesView: View {
    @Environment(\.widgetFamily) private var family
    let entry: NotesEntry

    private var lineBudget: Int {
        switch family {
        case .systemSmall: 2
        case .systemMedium: 4
        default: 9
        }
    }

    /// Written thoughts first (they match the diary lines); fall back to plain notes
    /// for a mirror made before memory.jsonl existed.
    private var rows: [Row] {
        let written = entry.snapshot.thoughts.filter(\.written)
        if !written.isEmpty {
            return written.prefix(lineBudget).map { Row(id: "t\($0.id)", time: $0.time, text: $0.thought, mood: $0,
                                                        changed: $0.changed.first, photo: AppGroup.photo($0.photo)) }
        }
        return entry.snapshot.notes.prefix(lineBudget).map { Row(id: $0.id, time: $0.time, text: $0.text, mood: nil,
                                                                 changed: nil, photo: nil) }
    }

    /// The newest photo among the shown rows — the medium and large widget lead
    /// with it, because a picture is why eve kept that moment.
    private var lead: (row: Row, url: URL)? {
        guard family != .systemSmall else { return nil }
        for row in rows { if let url = row.photo { return (row, url) } }
        return nil
    }

    private var debt: String? { entry.snapshot.newestDebt }

    /// How many thought cards fit under everything else in this family.
    private var visibleCount: Int {
        switch family {
        case .systemSmall: 1
        // Two cards of two lines each: three one-line cards cut every thought off mid-sentence.
        case .systemMedium: debt == nil ? 2 : 1
        case .systemExtraLarge: max(1, 6 - (debt == nil ? 0 : 1) - (lead == nil ? 0 : 1))
        default: max(1, 3 - (debt == nil ? 0 : 1) - (lead == nil ? 0 : 1))
        }
    }

    private var shown: [Row] { Array(rows.prefix(visibleCount)) }

    /// eve is stopped (or on its way there): the card sleeps instead of showing thoughts.
    private var off: Bool {
        switch entry.power {
        case .off, .turningOn, .turningOff, .waiting: true
        case .on, .unknown: false
        }
    }

    private var offLine: String {
        switch entry.power {
        case .turningOff: "Stopping the daemon…"
        case .turningOn: "Starting the daemon…"
        case .waiting: "Waiting for StackChan Notes, the menu-bar app. Open it and try again."
        default: "The daemon is stopped and stays stopped until you turn it on."
        }
    }

    private var count: Int {
        max(entry.snapshot.notes.count, entry.snapshot.thoughts.filter(\.written).count)
    }

    var body: some View {
        Group {
            switch family {
            case .systemSmall: small
            case .systemMedium: medium
            default: large
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .foregroundStyle(BrandStyle.ink)
        .environment(\.colorScheme, .light)
    }

    // MARK: small

    private var small: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                InkHeading("eve", size: 17)
                Spacer(minLength: 0)
                if off {
                    PowerPill(face: entry.power, size: 9.5)
                } else {
                    if let mood = entry.snapshot.mood { MoodPill(mood: mood, size: 9.5) }
                    PowerPill(face: entry.power, size: 9.5)
                }
            }
            if off {
                Spacer(minLength: 0)
                RobotFace(size: 54, compact: true)
                Text(offLine)
                    .font(BrandFont.hand(14))
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            } else if rows.isEmpty {
                Spacer(minLength: 0)
                RobotFace(size: 54, compact: true)
                Text("Nothing yet! eve looks around when Claude is resting.")
                    .font(BrandFont.hand(14))
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            } else {
                if let debt { DebtLine(text: debt, size: 13, lines: 2) }
                ForEach(shown) { row in
                    ThoughtRow(row: row, family: family, lines: debt == nil ? 3 : 2)
                }
                .padding(.trailing, 3)
                .padding(.bottom, 3)
                Spacer(minLength: 0)
            }
        }
    }

    // MARK: medium

    private var medium: some View {
        HStack(alignment: .top, spacing: 12) {
            // The left column: the photo or the face, with the power button under it.
            VStack(spacing: 6) {
                if !off, let lead, let image = NSImage(contentsOf: lead.url) {
                    PhotoStrip(image: image, shadow: .brandSun)
                        .frame(width: 89)
                        .padding(.trailing, 3)
                        .padding(.bottom, 3)
                } else {
                    RobotFace(size: 92, line: off ? "zzz" : rows.isEmpty ? "Hi!" : entry.snapshot.mood?.label)
                        .frame(maxHeight: .infinity)
                }
                PowerPill(face: entry.power, size: 9.5)
            }
            .frame(width: 92)

            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 6) {
                    // The name never gives up width; the pills after it shrink to icons first.
                    InkHeading("eve", size: 18)
                        .fixedSize()
                        .layoutPriority(1)
                    if !off, let mood = entry.snapshot.mood { MoodPill(mood: mood, size: 9.5) }
                    Spacer(minLength: 0)
                    ViewThatFits(in: .horizontal) {
                        HStack(spacing: 6) {
                            Link(destination: AppGroup.chatURL) { BrandPill("chat", fill: .brandPinkSoft, size: 9.5) }
                            Link(destination: learningURL) { BrandPill("lessons", fill: .brandSun, size: 9.5) }
                        }
                        HStack(spacing: 5) {
                            Link(destination: AppGroup.chatURL) { IconPill(symbol: "bubble.left.fill", fill: .brandPinkSoft, size: 9.5) }
                                .accessibilityLabel("chat")
                            Link(destination: learningURL) { IconPill(symbol: "book.fill", fill: .brandSun, size: 9.5) }
                                .accessibilityLabel("lessons")
                        }
                    }
                }
                if off {
                    Spacer(minLength: 0)
                    Text("eve is off.")
                        .font(BrandFont.display(16))
                    Text(offLine)
                        .font(BrandFont.body(12))
                        .foregroundStyle(BrandStyle.inkSoft)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                } else if rows.isEmpty {
                    Spacer(minLength: 0)
                    Text("Nothing noticed yet.")
                        .font(BrandFont.display(16))
                    Text("eve explores when Claude is resting.")
                        .font(BrandFont.body(12))
                        .foregroundStyle(BrandStyle.inkSoft)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                } else {
                    if let debt { DebtLine(text: debt, size: 14, lines: 1) }
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(shown) { row in ThoughtRow(row: row, family: family, lines: 2) }
                    }
                    .padding(.trailing, 3)
                    .padding(.bottom, 3)
                    Spacer(minLength: 0)
                }
            }
        }
    }

    // MARK: large

    private var large: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text("what eve noticed")
                    .font(BrandFont.hand(16))
                    .foregroundStyle(BrandStyle.inkSoft)
                Spacer(minLength: 0)
                Link(destination: AppGroup.chatURL) {
                    BrandPill("chat", fill: .brandPinkSoft)
                }
                Link(destination: learningURL) {
                    BrandPill("lessons", fill: .brandSun)
                }
                PowerPill(face: entry.power)
            }
            HStack(spacing: 8) {
                InkHeading("eve", size: 26)
                if !off, let mood = entry.snapshot.mood { MoodPill(mood: mood, size: 10.5) }
                Spacer(minLength: 0)
                BrandPill("\(count) notes")
            }
            if off {
                Spacer(minLength: 0)
                HStack(spacing: 14) {
                    RobotFace(size: 80, line: "zzz")
                    VStack(alignment: .leading, spacing: 4) {
                        Text("eve is off.")
                            .font(BrandFont.display(16))
                        Text(offLine)
                            .font(BrandFont.body(12))
                            .foregroundStyle(BrandStyle.inkSoft)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 0)
            } else if rows.isEmpty {
                Spacer(minLength: 0)
                HStack(spacing: 14) {
                    RobotFace(size: 80, line: "Hi!")
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Nothing noticed yet.")
                            .font(BrandFont.display(16))
                        Text("eve explores when Claude is resting.")
                            .font(BrandFont.body(12))
                            .foregroundStyle(BrandStyle.inkSoft)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 0)
            } else {
                if let debt {
                    DebtCard(text: debt)
                        .padding(.trailing, 4)
                        .padding(.bottom, 2)
                }
                if let lead, let image = NSImage(contentsOf: lead.url) {
                    PhotoStrip(image: image, shadow: .brandTeal)
                        .frame(height: 84)
                        .padding(.trailing, 3)
                        .padding(.bottom, 3)
                }
                VStack(alignment: .leading, spacing: 7) {
                    ForEach(shown) { row in ThoughtRow(row: row, family: family, lines: 2) }
                }
                .padding(.trailing, 3)
                .padding(.bottom, 3)
                Spacer(minLength: 0)
                Text("tap to open the diary")
                    .font(BrandFont.hand(14))
                    .foregroundStyle(BrandStyle.inkSoft)
            }
        }
    }
}

/// The power switch as a pill: a button that asks the helper to stop or start
/// eve (SetEvePowerIntent), a plain pill while that is in flight, nothing
/// when no helper has ever reported (this Mac has no service, or the helper is
/// too old to know about power).
private struct PowerPill: View {
    let face: EvePower.Face
    var size: CGFloat = 10.5

    var body: some View {
        switch face {
        case .unknown:
            EmptyView()
        case .on:
            Button(intent: SetEvePowerIntent(on: false)) { label("turn off", symbol: "power", fill: .brandSheet) }
                .buttonStyle(.plain)
                .accessibilityLabel("turn eve off")
        case .off:
            Button(intent: SetEvePowerIntent(on: true)) { label("turn on", symbol: "power", fill: .brandSun) }
                .buttonStyle(.plain)
                .accessibilityLabel("turn eve on")
        case .turningOff:
            label("stopping…", symbol: "hourglass", fill: .brandSheet)
        case .turningOn:
            label("starting…", symbol: "hourglass", fill: .brandSun)
        case .waiting(let on):
            // The helper never answered. Pressing again sends the same request with a fresh stamp,
            // which a helper that has started since will serve.
            Button(intent: SetEvePowerIntent(on: on)) { label("try again", symbol: "arrow.clockwise", fill: .brandSheet) }
                .buttonStyle(.plain)
                .accessibilityLabel(on ? "try turning eve on again" : "try turning eve off again")
        }
    }

    /// The words when they fit, the symbol alone when they do not.
    private func label(_ text: String, symbol: String, fill: Color) -> some View {
        ViewThatFits(in: .horizontal) {
            BrandPill(text, fill: fill, size: size)
            IconPill(symbol: symbol, fill: fill, size: size)
        }
    }
}

/// A pill holding only a symbol, for when a word pill does not fit.
private struct IconPill: View {
    let symbol: String
    let fill: Color
    var size: CGFloat = 10.5

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: size + 1, weight: .bold))
            .foregroundStyle(BrandStyle.ink)
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .background(Capsule().fill(BrandStyle.fill(fill)))
            .overlay(Capsule().strokeBorder(BrandStyle.ink, lineWidth: 1.5))
    }
}

/// The feeling as a pill: emoji and label, or the label alone when space is short.
private struct MoodPill: View {
    let mood: Thought
    var size: CGFloat = 10.5

    var body: some View {
        ViewThatFits(in: .horizontal) {
            BrandPill("\(MoodColor.emoji(for: mood.label)) \(mood.label)", fill: BrandMood.color(mood.label), size: size)
            BrandPill(mood.label, fill: BrandMood.color(mood.label), size: size)
            Text(MoodColor.emoji(for: mood.label)).font(.system(size: size + 3))
        }
        .accessibilityLabel("feeling \(mood.label)")
    }
}

/// A debt of eve's own, as a handwritten line.
private struct DebtLine: View {
    let text: String
    let size: CGFloat
    let lines: Int

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 4) {
            Image(systemName: "arrow.uturn.backward")
                .font(.system(size: 9, weight: .bold))
                .foregroundStyle(BrandStyle.pink)
            Text(text)
                .font(BrandFont.hand(size))
                .foregroundStyle(BrandStyle.inkSoft)
                .lineLimit(lines)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// A debt of eve's own, as a sun-coloured card pinned slightly askew (large only).
private struct DebtCard: View {
    let text: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "arrow.uturn.backward")
                .font(.system(size: 10, weight: .bold))
            Text(text)
                .font(BrandFont.body(12, bold: true))
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .foregroundStyle(BrandStyle.ink)
        .padding(.horizontal, 9)
        .padding(.vertical, 6)
        .paperCard(shadow: .brandPink, offset: 3, fill: .brandSun)
        .rotationEffect(.degrees(-0.6))
    }
}

/// The mirrored photo, cropped to fill its frame, as a cut-paper print.
private struct PhotoStrip: View {
    let image: NSImage
    let shadow: Color

    var body: some View {
        Color.brandSheet
            .overlay {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
            }
            .clipShape(RoundedRectangle(cornerRadius: 5))
            .overlay(alignment: .topTrailing) {
                Image(systemName: "camera.fill")
                    .font(.system(size: 8, weight: .bold))
                    .foregroundStyle(BrandStyle.ink)
                    .padding(4)
                    .background(Circle().fill(BrandStyle.fill(.brandSheet)))
                    .overlay(Circle().strokeBorder(BrandStyle.ink, lineWidth: 1.5))
                    .padding(5)
            }
            .paperCard(shadow: shadow, offset: 3, fill: .clear)
            .accessibilityLabel("a photo eve kept")
    }
}

struct Row: Identifiable {
    let id: String
    let time: String
    let text: String
    let mood: Thought?
    let changed: String?
    /// The mirrored photo eve kept for this thought, if it kept one.
    var photo: URL? = nil
}

/// One thought as a card; the offset shadow is the colour of its feeling.
private struct ThoughtRow: View {
    let row: Row
    let family: WidgetFamily
    let lines: Int

    private var time: some View {
        Text(row.time)
            .font(BrandFont.body(10, bold: true))
            .monospacedDigit()
            .foregroundStyle(BrandStyle.inkSoft)
    }

    private var text: some View {
        Text(row.text)
            .font(BrandFont.body(family == .systemSmall ? 12 : 12.5))
            .foregroundStyle(BrandStyle.ink)
            .lineLimit(lines)
            .truncationMode(.tail)
            .fixedSize(horizontal: false, vertical: true)
    }

    var body: some View {
        Group {
            switch family {
            case .systemSmall:
                VStack(alignment: .leading, spacing: 1) {
                    time
                    text
                }
            case .systemMedium:
                HStack(alignment: .firstTextBaseline, spacing: 7) {
                    time
                    text
                }
            default:
                VStack(alignment: .leading, spacing: 2) {
                    HStack(alignment: .center, spacing: 6) {
                        time
                        if let m = row.mood {
                            BrandPill(m.label, fill: BrandMood.color(m.label), size: 9)
                        }
                        if let changed = row.changed {
                            Text("changed: \(changed)")
                                .font(BrandFont.body(10.5))
                                .foregroundStyle(BrandStyle.inkSoft)
                                .lineLimit(1)
                        }
                    }
                    text
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 5)
        .padding(.horizontal, 9)
        // A plain note (no feeling recorded) still gets a visible paper shadow.
        .paperCard(shadow: row.mood == nil ? .brandSky : Color.mood(row.mood), offset: 3)
    }
}

#Preview("medium", as: .systemMedium) {
    StackChanNotesWidget()
} timeline: {
    NotesEntry(date: .now, snapshot: .placeholder)
    NotesEntry(date: .now, snapshot: .empty)
}


struct LearningEntry: TimelineEntry {
    let date: Date
    let snapshot: LearningSnapshot
}

struct LearningProvider: TimelineProvider {
    func placeholder(in context: Context) -> LearningEntry {
        LearningEntry(date: .now, snapshot: .empty)
    }
    func getSnapshot(in context: Context, completion: @escaping (LearningEntry) -> Void) {
        completion(LearningEntry(date: .now, snapshot: LearningSnapshot.load()))
    }
    func getTimeline(in context: Context, completion: @escaping (Timeline<LearningEntry>) -> Void) {
        completion(Timeline(entries: [LearningEntry(date: .now, snapshot: LearningSnapshot.load())],
                            policy: .after(.now.addingTimeInterval(15 * 60))))
    }
}

struct EveLearningWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "EveLearning", provider: LearningProvider()) { entry in
            LearningCardView(entry: entry)
                .containerBackground(for: .widget) { LearningCardBackground() }
                .widgetURL(learningURL)
        }
        .configurationDisplayName("eve's learning")
        .description("Your saved lessons with eve. Tap to keep learning.")
        .supportedFamilies([.systemMedium, .systemLarge])
    }
}

/// The paper the learning widget is drawn on.
struct LearningCardBackground: View {
    var body: some View { Color.brandPaper }
}

struct LearningCardView: View {
    let entry: LearningEntry
    @Environment(\.widgetFamily) private var family

    private var isLarge: Bool { family == .systemLarge }

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            if isLarge {
                Text("let's learn!")
                    .font(BrandFont.hand(16))
                    .foregroundStyle(BrandStyle.inkSoft)
            }
            HStack(spacing: 6) {
                InkHeading("eve's learning", size: isLarge ? 22 : 18)
                Spacer(minLength: 4)
                BrandPill("\(entry.snapshot.total) saved", size: isLarge ? 10.5 : 9.5)
                BrandPill("\(entry.snapshot.completed) done", fill: .brandTeal, size: isLarge ? 10.5 : 9.5)
            }
            if entry.snapshot.lessons.isEmpty {
                Spacer(minLength: 0)
                HStack(spacing: 14) {
                    RobotFace(size: isLarge ? 110 : 80, line: "Hey there!")
                    Text("Big ideas start with little steps. Tap to start a lesson.")
                        .font(BrandFont.body(isLarge ? 15 : 13))
                        .foregroundStyle(BrandStyle.ink)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
            } else {
                VStack(alignment: .leading, spacing: isLarge ? 9 : 6) {
                    ForEach(entry.snapshot.lessons.prefix(isLarge ? 3 : 2)) { lesson in
                        LessonRow(lesson: lesson, roomy: isLarge)
                    }
                }
                .padding(.trailing, 3)
                .padding(.bottom, 3)
                Spacer(minLength: 0)
            }
            if isLarge {
                Text("Open my lessons →")
                    .font(BrandFont.body(13, bold: true))
                    .foregroundStyle(BrandStyle.ink)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 5)
                    .paperCard(shadow: .brandInk, offset: 3, radius: 8, fill: .brandSun)
                    .padding(.trailing, 3)
                    .padding(.bottom, 3)
                    .accessibilityAddTraits(.isLink)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .foregroundStyle(BrandStyle.ink)
        .environment(\.colorScheme, .light)
    }
}

/// One saved lesson as a card: teal when done, sun while eve and the learner are on it.
private struct LessonRow: View {
    let lesson: LearningLesson
    let roomy: Bool

    private var done: Bool { lesson.stage == "complete" }

    var body: some View {
        HStack(spacing: 8) {
            BrandPill(done ? "done" : "on it", fill: done ? .brandTeal : .brandSun, size: 9.5)
                .frame(minWidth: 52, alignment: .leading)
            VStack(alignment: .leading, spacing: 0) {
                Text(lesson.topic)
                    .font(BrandFont.body(13, bold: true))
                    .foregroundStyle(BrandStyle.ink)
                    .lineLimit(1)
                if !lesson.problem.isEmpty {
                    Text(lesson.problem)
                        .font(BrandFont.body(11.5))
                        .foregroundStyle(BrandStyle.inkSoft)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, roomy ? 6 : 3)
        .padding(.horizontal, 9)
        .paperCard(shadow: done ? .brandTeal : .brandSun, offset: 3)
    }
}
