// The home-screen widget (docs/widget.md): what needs the owner, what is going, what is left today and what plays,
// from one read of the daemon's GET /api/link/today (Shared/Today.swift). Large is the command center; medium is
// the top four rows. Minimal, like the app icon (owner, 2026-10-08): white on black, eve's two pill eyes as the only
// mark, plain symbols, one line per row. The one colour is the app's blue dot beside "Needs you".
import SwiftUI
import WidgetKit

struct TodayEntry: TimelineEntry {
    var date: Date
    var snapshot: TodaySnapshot
}

struct TodayProvider: TimelineProvider {
    static let refresh: TimeInterval = 15 * 60
    /// After a failed read: eve restarting takes about 30 s to post its new tunnel address (stick_link.py), and a
    /// read in that gap used to show "Can't reach the Mac." for the full 15 minutes (owner, 2026-10-08).
    static let retry: TimeInterval = 2 * 60

    func placeholder(in context: Context) -> TodayEntry {
        TodayEntry(date: Date(), snapshot: TodaySnapshot(feed: TodaySample.feed(Date()), fetched: Date(), problem: nil))
    }

    func getSnapshot(in context: Context, completion: @escaping @Sendable (TodayEntry) -> Void) {
        if context.isPreview {
            completion(placeholder(in: context))
            return
        }
        Task { completion(TodayEntry(date: Date(), snapshot: await Self.load())) }
    }

    func getTimeline(in context: Context, completion: @escaping @Sendable (Timeline<TodayEntry>) -> Void) {
        Task {
            let now = Date()
            let snapshot = await Self.load()
            // one entry now, and one at each event start or end in the next refresh window, so "Now" and the list
            // of events stay right between reads
            let next = now.addingTimeInterval(snapshot.problem == .unreachable ? Self.retry : Self.refresh)
            let marks = (snapshot.feed?.events ?? []).flatMap { [$0.start, $0.end] }.filter { $0 > now && $0 < next }
            let entries = ([now] + Set(marks).sorted()).map { TodayEntry(date: $0, snapshot: snapshot) }
            completion(Timeline(entries: entries, policy: .after(next)))
        }
    }

    static func load() async -> TodaySnapshot {
        let session = URLSession(configuration: .ephemeral)
        return await TodayLoader.load(
            pairing: Keychain.load(), cache: .standard,
            fetch: { request in
                let (data, response) = try await session.data(for: request)
                return (data, (response as? HTTPURLResponse)?.statusCode ?? 0)
            },
            rendezvous: { token in await Rendezvous.latest(token: token) },
            repaired: { Keychain.save($0) })
    }
}

struct TodayWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: TodayWidgetKind.kind, provider: TodayProvider()) { entry in
            TodayView(entry: entry)
        }
        .configurationDisplayName("eve")
        .description("What needs you, what's going, what's left today, and what's playing.")
        .supportedFamilies([.systemLarge, .systemMedium])
    }
}

private enum Ink {
    static let ink = Color.white
    static let quiet = Color.white.opacity(0.5)
    static let faint = Color.white.opacity(0.32)
    static let screen = Color.black
    static let needsYou = Color(red: 10 / 255, green: 132 / 255, blue: 1)     // the app's dot for a job waiting on you
}

/// eve's face: two white pill eyes, as on the app icon.
private struct TodayEyes: View {
    var size: CGFloat = 12

    var body: some View {
        HStack(spacing: size * 0.42) {
            Capsule().fill(Ink.ink).frame(width: size, height: size * 0.5)
            Capsule().fill(Ink.ink).frame(width: size, height: size * 0.5)
        }
        .accessibilityHidden(true)
    }
}

struct TodayView: View {
    let entry: TodayEntry
    @Environment(\.widgetFamily) private var family

    var body: some View {
        TodayFace(entry: entry, large: family != .systemMedium)
    }
}

/// The widget's face for one size (the size is a parameter, so the design checks can draw both outside a widget).
struct TodayFace: View {
    let entry: TodayEntry
    let large: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if let feed = entry.snapshot.feed, entry.snapshot.problem != .unpaired {
                if feed.isEmpty(at: entry.date) {
                    Spacer(minLength: 0)
                    Calm(text: "Nothing on. eve's watching.")
                    Spacer(minLength: 0)
                } else if large {
                    Board(feed: feed, date: entry.date)
                    Spacer(minLength: 0)
                } else {
                    Strip(feed: feed, date: entry.date)
                    Spacer(minLength: 0)
                }
            } else {
                Spacer(minLength: 0)
                Calm(text: entry.snapshot.problem?.line ?? "Can't reach the Mac.")
                Spacer(minLength: 0)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .containerBackground(for: .widget) { Ink.screen }
        .widgetURL(TodayWidgetKind.workURL)
        .environment(\.colorScheme, .dark)
    }

    /// The eyes, and a quiet "as of 5:42" while the data is old (the Mac did not answer the last read).
    private var header: some View {
        HStack(spacing: 8) {
            TodayEyes(size: large ? 13 : 11)
            if !large, let feed = entry.snapshot.feed, feed.needsYou > 0, entry.snapshot.problem != .unpaired {
                Text("\(feed.needsYou) need\(feed.needsYou == 1 ? "s" : "") you")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Ink.quiet)
            }
            Spacer(minLength: 4)
            if entry.snapshot.stale, let when = entry.snapshot.fetched {
                Text("as of \(when.formatted(.dateTime.hour(.defaultDigits(amPM: .omitted)).minute()))")
                    .font(.system(size: 11))
                    .foregroundStyle(Ink.faint)
                    .lineLimit(1)
            }
        }
        .frame(height: 16)
        .padding(.bottom, large ? 10 : 8)
    }
}

/// One centered line under the eyes: the calm empty state, or why there is nothing to show.
private struct Calm: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 14))
            .foregroundStyle(Ink.quiet)
            .lineLimit(1)
            .minimumScaleFactor(0.8)
            .frame(maxWidth: .infinity)
    }
}

/// The large widget: headed sections.
private struct Board: View {
    let feed: TodayFeed
    let date: Date
    static let budget = 12        // lines that fit a large widget, a heading counting as one

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            ForEach(TodayLayout.sections(feed, at: date, budget: Self.budget), id: \.kind) { section in
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 5) {
                        Text(section.title)
                        if section.kind == .needs {
                            Circle().fill(Ink.needsYou).frame(width: 5, height: 5)
                        }
                        Spacer(minLength: 4)
                        if section.kind == .needs {
                            Text("\(section.count)").monospacedDigit()
                        } else if section.more > 0 {
                            Text("+\(section.more)").monospacedDigit()
                        }
                    }
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Ink.quiet)
                    ForEach(Array(section.lines.enumerated()), id: \.offset) { _, line in
                        Row(line: line)
                    }
                }
            }
        }
    }
}

/// The medium widget: the top rows, no headings.
private struct Strip: View {
    let feed: TodayFeed
    let date: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(TodayLayout.flat(feed, at: date, rows: 4).enumerated()), id: \.offset) { _, line in
                Row(line: line)
            }
        }
    }
}

/// One line: a symbol or a time, the title, and a quiet detail at the right.
private struct Row: View {
    let line: TodayLine

    var body: some View {
        HStack(spacing: 8) {
            lead
                .frame(width: 34, alignment: .leading)
            Text(line.title)
                .font(.system(size: 14))
                .foregroundStyle(Ink.ink)
                .lineLimit(1)
                .layoutPriority(1)
            Spacer(minLength: 6)
            if !line.detail.isEmpty {
                Text(line.detail)
                    .font(.system(size: 12))
                    .foregroundStyle(Ink.quiet)
                    .lineLimit(1)
            }
        }
        .frame(height: 18)
    }

    @ViewBuilder private var lead: some View {
        switch line.lead {
        case .symbol(let name):
            Image(systemName: name)
                .font(.system(size: 12, weight: .regular))
                .foregroundStyle(Ink.quiet)
        case .time(let at):
            Text(at.formatted(.dateTime.hour(.defaultDigits(amPM: .omitted)).minute()))
                .font(.system(size: 12).monospacedDigit())
                .foregroundStyle(Ink.quiet)
        case .now:
            Text("now")
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Ink.ink)
        }
    }
}

/// The gallery's preview and the design checks: a made-up day, no real names.
enum TodaySample {
    static func feed(_ now: Date) -> TodayFeed {
        TodayFeed(
            at: now, needsYou: 2,
            needs: [TodayRow(id: "c4", kind: "job", title: "Book the car service", line: "Waiting on your Go"),
                    TodayRow(id: "c7", kind: "job", title: "Renew the parking permit", line: "Did it go through?")],
            going: [TodayRow(id: "c5", kind: "job", title: "Compare flights for Friday", line: "Researching"),
                    TodayRow(id: "w1", kind: "watch", title: "Desk chair", line: "$320.00"),
                    TodayRow(id: "w2", kind: "watch", title: "Concert tickets", line: "not available")],
            events: [TodayEvent(title: "Design review", start: now.addingTimeInterval(-900), end: now.addingTimeInterval(1800)),
                     TodayEvent(title: "Dentist", start: now.addingTimeInterval(7200), end: now.addingTimeInterval(10800)),
                     TodayEvent(title: "Dinner", start: now.addingTimeInterval(18000), end: now.addingTimeInterval(23400))],
            now: [TodayRow(kind: "music", title: "A song", line: "Someone"),
                  TodayRow(kind: "tv", title: "TV", line: "YouTube")])
    }
}

#Preview("Large", as: .systemLarge) {
    TodayWidget()
} timeline: {
    TodayEntry(date: .now, snapshot: TodaySnapshot(feed: TodaySample.feed(.now), fetched: .now, problem: nil))
    TodayEntry(date: .now, snapshot: TodaySnapshot(feed: TodaySample.feed(.now), fetched: .now.addingTimeInterval(-3600),
                                                   problem: .unreachable))
    TodayEntry(date: .now, snapshot: TodaySnapshot(
        feed: TodayFeed(at: .now, needsYou: 0, needs: [], going: [], events: [], now: []), fetched: .now, problem: nil))
}

#Preview("Medium", as: .systemMedium) {
    TodayWidget()
} timeline: {
    TodayEntry(date: .now, snapshot: TodaySnapshot(feed: TodaySample.feed(.now), fetched: .now, problem: nil))
    TodayEntry(date: .now, snapshot: TodaySnapshot(feed: nil, fetched: nil, problem: .unpaired))
}
