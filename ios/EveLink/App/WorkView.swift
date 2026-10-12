// The Apps and Work tabs (docs/app-chat.md): one plain native list each. Work is Claude Code sessions on the Mac
// (docs/work-sessions.md; owner, 2026-10-12: "can work essentially be a new terminal session"): each row a session's
// state, folder, last line and last activity, refreshed while the tab is open; "+" starts one in the work folder with
// the typed task (NewSessionSheet), and a session opens SessionSheet.swift. The chief's jobs stay below as a small
// section while there are any (the menu's dot and Settings still point here). Apps is Notes (the owner's vault,
// NotesView.swift) and then the apps the Mac's registry lists, in its order (app_registry.py; ControlsInfo.rows): the
// owner's own (Music, Lights, Remote, Pika, Camera) open their native sheets (ControlSheets.swift), any other app the
// generic sheet (GenericAppSheet.swift). Memory is in Settings (owner, 2026-10-10). The owner's built apps are not
// listed (owner, 2026-10-08); the owner asked for "chat, apps, work" as three tabs the same evening.
import SwiftUI

@MainActor
final class WorkModel: ObservableObject {
    @Published private(set) var jobs: WorkJobs?
    @Published private(set) var jobsProblem: String?
    /// Which of eve's controls this Mac has.
    @Published private(set) var controls: ControlsInfo?
    @Published private(set) var controlsProblem: String?
    /// Jobs waiting on the owner: the menu button's blue dot.
    @Published private(set) var needsYou = 0
    /// An action the owner took landed: the Go haptic (and a light one for the others).
    @Published private(set) var acted = 0
    /// The Claude Code sessions on the Mac (WorkSessions.swift).
    @Published private(set) var sessions: WorkSessionList?
    @Published private(set) var sessionsProblem: String?

    /// The sheets' calls go through the same client (ControlsClient.swift).
    let client: WorkClient

    init(relay: Relay) {
        client = WorkClient(
            endpoint: { [weak relay] in relay?.pairing.map { ChatEndpoint(base: $0.base, token: $0.token) } },
            refresh: { [weak relay] in await relay?.refreshAddress() ?? false })
    }

    func refresh() async {
        async let j: Void = refreshJobs()
        async let a: Void = refreshControls()
        _ = await (j, a)
    }

    func refreshJobs() async {
        switch await client.jobs() {
        case .success(let got):
            if got != jobs, jobs != nil { HomeWidget.reload() }    // a job came, went or moved: the widget too
            jobs = got
            needsYou = got.needsYou
            jobsProblem = nil
        case .failure(let f):
            jobsProblem = f.message
            needsYou = 0
        }
    }

    func refreshControls() async {
        switch await client.controls() {
        case .success(let got):
            controls = got
            controlsProblem = nil
        case .failure(let f):
            controlsProblem = f.message
        }
    }

    func refreshSessions() async {
        switch await client.sessions() {
        case .success(let got):
            if got != sessions { sessions = got }
            sessionsProblem = nil
        case .failure(let f):
            sessionsProblem = f.message
        }
    }

    struct Problem: Error { let message: String }

    /// + New: one Claude Code session in the work folder, the task its first prompt. The session as started, or why
    /// there is none. The same clientID again is the same session (a retry never opens a second terminal).
    func startSession(task: String, clientID: String) async -> Result<WorkSession, Problem> {
        let out: Result<WorkSession, Problem>
        switch await client.startSession(task: task, clientID: clientID) {
        case .success(let r):
            if let s = r.session {
                out = .success(s)
                acted += 1
            } else {
                out = .failure(Problem(message: r.reason ?? "eve didn't start a session."))
            }
        case .failure(let f):
            out = .failure(Problem(message: f.message))
        }
        await refreshSessions()
        return out
    }

    /// A session's latest turns, for its sheet.
    func detail(_ id: String) async -> Result<WorkSessionDetail, Problem> {
        switch await client.session(id) {
        case .success(let d): .success(d)
        case .failure(let f): .failure(Problem(message: f.message))
        }
    }

    /// The owner's words typed into that session's terminal. nil when they landed, else why not.
    func reply(_ text: String, to id: String) async -> String? {
        switch await client.reply(text, to: id) {
        case .success(let r) where r.ok:
            acted += 1
            return nil
        case .success(let r): return r.reason ?? "eve didn't send that."
        case .failure(let f): return f.message
        }
    }

    /// Ends the session (Claude quits; its terminal stays). nil when it is stopped or was already gone.
    func stop(_ id: String) async -> String? {
        let said: String?
        switch await client.stopSession(id) {
        case .success(let r) where r.ok:
            said = nil
            acted += 1
        case .success(let r): said = r.reason ?? "eve didn't stop it."
        case .failure(let f): said = f.message
        }
        await refreshSessions()
        return said
    }

    /// The owner's action on a job. nil when it went through, else the reason in words.
    func act(_ action: WorkAction, on job: WorkJob) async -> String? {
        let said: String?
        switch await client.act(action, job: job.id, rev: job.rev) {
        case .success(let r) where r.ok:
            said = nil
            acted += 1
        case .success(let r):
            said = r.reason ?? "eve didn't take that."
        case .failure(let f):
            said = f.message
        }
        await refreshJobs()
        return said
    }
}

struct WorkView: View {
    @ObservedObject var work: WorkModel
    /// Which tab this is: .apps lists the controls, anything else the jobs.
    let shows: HomeMode
    @State private var job: WorkJob?
    @State private var control: AppRow?
    @State private var session: WorkSession?
    @State private var newSession = false
    /// The session + New started: its sheet opens once the New session sheet has gone.
    @State private var started: WorkSession?
    /// How often the open Work tab reads the sessions again.
    static let sessionPoll: Duration = .seconds(6)

    var body: some View {
        VStack(spacing: 0) {
            if shows == .work { workBar }
            List {
                if shows == .apps { controlRows } else { sessionRows; jobRows }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
        }
        .background(Color.eveBackground)
        .refreshable { await reload() }
        .task(id: shows) {
            await reload()
            #if DEBUG && targetEnvironment(simulator)
            if shows == .apps, let raw = DesignHook.control ?? (DesignHook.screen == "pika" ? "pika" : nil) {
                // a row the Mac listed (a generic app too), else a native sheet by name (Memory, from Settings)
                if let row = work.controls?.rows.first(where: { $0.id == raw }) { control = row }
                else if let kind = ControlKind(rawValue: raw) { control = .native(kind) }
            }
            #endif
            // Work: the sessions again every few seconds while the tab is open (the task ends with the tab).
            while shows == .work, !Task.isCancelled {
                try? await Task.sleep(for: Self.sessionPoll)
                if Task.isCancelled { break }
                if session == nil { await work.refreshSessions() }   // an open sheet reads its own session
            }
        }
        .sheet(item: $job) { shown in
            JobSheet(work: work, jobID: shown.id, fallback: shown)
        }
        .sheet(item: $session, onDismiss: { Task { await work.refreshSessions() } }) { shown in
            SessionSheet(work: work, fallback: shown)
        }
        .sheet(item: $control) { row in
            switch row {
            case .native(let kind): ControlSheet(kind: kind, client: work.client, lights: work.controls?.lights ?? [])
            case .generic(let entry): GenericAppSheet(entry: entry, client: work.client)
            }
        }
        .sheet(isPresented: $newSession, onDismiss: {
            if let s = started {
                started = nil
                session = s
            }
        }) {
            NewSessionSheet(work: work) { s in started = s }
        }
    }

    /// The Work list's own toolbar: what it lists, and a new session.
    private var workBar: some View {
        HStack {
            Text("Sessions").font(.headline)
            Spacer()
            Button { newSession = true } label: {
                Image(systemName: "plus").font(.system(size: 16, weight: .semibold))
            }
            .buttonStyle(RoundButtonStyle(size: 36))
            .foregroundStyle(.primary)
            .accessibilityLabel("New session")
            .accessibilityHint("Starts Claude Code on your Mac with a task")
        }
        .padding(.horizontal, 20)
        .padding(.top, 8)
        .padding(.bottom, 4)
    }

    private func reload() async {
        if shows == .apps {
            await work.refreshControls()
        } else {
            async let s: Void = work.refreshSessions()
            async let j: Void = work.refreshJobs()
            _ = await (s, j)
        }
    }

    @ViewBuilder private var sessionRows: some View {
        if let list = work.sessions, !list.sessions.isEmpty {
            ForEach(list.sessions) { s in
                Button { session = s } label: {
                    TimelineView(.periodic(from: .now, by: 30)) { clock in    // "2 min ago" moves on between reads
                        SessionRow(session: s, now: clock.date)
                    }
                }
                .listRowBackground(Color.eveBackground)
            }
        } else if let problem = work.sessionsProblem {
            quiet(problem)
        } else if work.sessions == nil {
            quiet("Loading…")
        } else {
            quiet("No sessions yet. Tap + to start Claude Code on your Mac.")
        }
    }

    /// The chief's jobs, a small section under the sessions while there are any (the chat can still file one).
    @ViewBuilder private var jobRows: some View {
        if let jobs = work.jobs, !jobs.jobs.isEmpty {
            Section {
                ForEach(jobs.open + jobs.finished) { j in
                    Button { job = j } label: { JobRow(job: j) }
                        .listRowBackground(Color.eveBackground)
                        .contextMenu {                     // a long press: a sideways swipe changes the pane
                            if j.actions.contains(.hide) {     // a finished job: gone from Work for good
                                Button("Delete", systemImage: "trash", role: .destructive) {
                                    Task { _ = await work.act(.hide, on: j) }
                                }
                            }
                        }
                }
            } header: {
                Text("Jobs").font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var controlRows: some View {
        if let controls = work.controls, !controls.rows.isEmpty {
            ForEach(controls.rows) { row in
                Button { control = row } label: { ControlRow(row: row, info: controls) }
                    .listRowBackground(Color.eveBackground)
            }
        } else {
            quiet(work.controlsProblem ?? (work.controls == nil ? "Loading…" : "No notes or apps on this Mac."))
        }
    }

    private func quiet(_ line: String) -> some View {
        Text(line)
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .listRowBackground(Color.eveBackground)
            .listRowSeparator(.hidden)
    }
}

private struct JobRow: View {
    let job: WorkJob

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                if job.needsYou {
                    Circle().fill(Color.blue).frame(width: 8, height: 8).accessibilityHidden(true)
                }
                Text(job.title)
                    .font(.body)
                    .foregroundStyle(job.open ? .primary : .secondary)
                    .lineLimit(2)
            }
            Text(job.statusLine)
                .font(.subheadline)
                .foregroundStyle(.secondary)
            if !job.steps.isEmpty {
                StepLine(progress: job.progress)
            }
        }
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(job.title). \(job.statusLine)."
                            + (job.steps.isEmpty ? "" : " \(job.stepsDone) of \(job.steps.count) steps done."))
        .accessibilityHint("Opens the job")
        .accessibilityAddTraits(.isButton)
    }
}

/// The thin line of steps done.
private struct StepLine: View {
    let progress: Double

    var body: some View {
        GeometryReader { g in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.eveFill)
                Capsule().fill(Color.primary.opacity(0.75)).frame(width: max(0, min(1, progress)) * g.size.width)
            }
        }
        .frame(height: 3)
        .accessibilityHidden(true)
    }
}

/// One app: its symbol in a rounded square, its name, and what it reaches. The Mac's words when it sent them (the
/// registry's title, SF Symbol and detail), else the phone's own for a native app.
private struct ControlRow: View {
    let row: AppRow
    let info: ControlsInfo

    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: symbol)
                .font(.system(size: 17, weight: .medium))
                .foregroundStyle(.primary)
                .frame(width: 36, height: 36)
                .background(RoundedRectangle(cornerRadius: 9, style: .continuous).fill(Color.eveFill))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.body).foregroundStyle(.primary)
                if !detail.isEmpty {
                    Text(detail).font(.subheadline).foregroundStyle(.secondary)
                }
            }
            Spacer()
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(detail.isEmpty ? title : "\(title). \(detail)")
        .accessibilityHint("Opens the \(title.lowercased()) controls")
        .accessibilityAddTraits(.isButton)
    }

    /// The Mac's entry for this row, if it sent one.
    private var sent: AppEntry? {
        switch row {
        case .native(let kind): info.entry(for: kind)
        case .generic(let entry): entry
        }
    }

    private var title: String {
        if let sent { return sent.title }
        if case .native(let kind) = row { return kind.title }
        return ""
    }

    private var symbol: String {
        if case .native(let kind) = row { return AppSymbol.named(sent?.symbol, fallback: kind.symbol) }
        return AppSymbol.named(sent?.symbol)
    }

    private var detail: String {
        if let sent, !sent.detail.isEmpty { return sent.detail }
        guard case .native(let kind) = row else { return "" }
        return switch kind {
        case .spotify: "Spotify"
        case .lights: info.lights.count == 1 ? "1 light" : "\(info.lights.count) lights"
        case .firetv: "Fire TV"
        case .memory: "What eve remembers"
        case .notes: "Your second brain"
        case .pika: "Drive the car"
        case .obsbot: "OBSBOT webcam"
        }
    }
}

/// One session: what it is doing in a chip, its title, what it last said, its folder and when it last moved.
private struct SessionRow: View {
    let session: WorkSession
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                SessionChip(state: session.state)
                Spacer(minLength: 0)
                Text(WorkSession.ago(session.lastActivity, now: now))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            Text(session.headline)
                .font(.body)
                .foregroundStyle(.primary)
                .lineLimit(2)
            if !session.subline.isEmpty {
                Text(session.subline)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            if !session.folder.isEmpty {
                Label(session.folder, systemImage: "folder")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .labelStyle(.titleAndIcon)
            }
        }
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityText)
        .accessibilityHint("Opens the session")
        .accessibilityAddTraits(.isButton)
    }

    private var accessibilityText: String {
        var parts = [session.headline, session.state.label]
        if !session.subline.isEmpty { parts.append(session.subline) }
        if !session.folder.isEmpty { parts.append("In \(session.folder)") }
        let ago = WorkSession.ago(session.lastActivity, now: now)
        if !ago.isEmpty { parts.append("Last active \(ago)") }
        return parts.joined(separator: ". ")
    }
}

/// A session's state as a small capsule: blue when it waits on the owner, the text color while it works, muted when
/// it is idle or gone. The word is always there, so the color is never the only cue.
struct SessionChip: View {
    let state: WorkSessionState

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(tint).frame(width: 7, height: 7)
            Text(state.label).font(.caption.weight(.semibold))
        }
        .foregroundStyle(state == .waiting ? Color.blue : state.rank <= 1 ? Color.primary : Color.secondary)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(Capsule().fill(Color.eveFill))
        .overlay(Capsule().strokeBorder(Color.eveEdge, lineWidth: 1))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(state.label)
    }

    private var tint: Color {
        switch state {
        case .waiting: .blue
        case .working, .starting: .green
        case .idle, .ended: .secondary
        }
    }
}

// ---- a new session (owner, 2026-10-12: "a new terminal session in documents personal") -------------------------------

/// The task in the owner's words and Start: Claude Code opens in a new terminal on the Mac, in the work folder, with
/// the task as its first prompt.
private struct NewSessionSheet: View {
    @ObservedObject var work: WorkModel
    /// The session it started: the caller opens its sheet once this one has gone.
    let started: (WorkSession) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var task = ""
    @State private var running = false
    @State private var problem: String?
    /// One per wording: a retry after a dropped connection is the same session, never a second terminal; changed
    /// words are a new one.
    @State private var clientID = NewSessionSheet.freshID()
    @FocusState private var focused: Bool

    private static func freshID() -> String { "ws-" + UUID().uuidString.lowercased() }
    private var canStart: Bool { !running && !task.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    TextField("What should Claude work on?", text: $task, axis: .vertical)
                        .lineLimit(4...12)
                        .focused($focused)
                        .accessibilityLabel("Task")
                } header: {
                    Text("Task")
                } footer: {
                    Text("Claude Code starts in a new terminal on your Mac, in your \(folder) folder. "
                         + "You can follow it here, reply and stop it.")
                }
                if task.count > WorkSessionRequests.maxTask - 400 {
                    Section {
                        Text("\(min(task.count, WorkSessionRequests.maxTask)) of \(WorkSessionRequests.maxTask) characters")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                }
                if let problem {
                    Section { Text(problem).foregroundStyle(.red).font(.subheadline) }
                }
            }
            .disabled(running)
            .navigationTitle("New session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    if running {
                        ProgressView().accessibilityLabel("Starting")
                    } else {
                        Button("Start") { Task { await start() } }
                            .fontWeight(.semibold)
                            .disabled(!canStart)
                    }
                }
            }
            .onAppear { focused = true }
            .onChange(of: task) { _, now in
                if now.count > WorkSessionRequests.maxTask { task = String(now.prefix(WorkSessionRequests.maxTask)) }
                clientID = Self.freshID()
            }
        }
        .presentationDragIndicator(.visible)
        .interactiveDismissDisabled(running)
        .sensoryFeedback(.success, trigger: work.acted)
    }

    private var folder: String {
        let name = work.sessions?.folder ?? ""
        return name.isEmpty ? "work" : name
    }

    private func start() async {
        guard canStart else { return }
        running = true
        problem = nil
        let out = await work.startSession(task: task, clientID: clientID)
        running = false
        switch out {
        case .success(let s):
            started(s)
            dismiss()
        case .failure(let p):
            problem = p.message                 // the same words again ask for the same session (clientID)
        }
    }
}

// ---- a job ---------------------------------------------------------------------------------------------------------

private struct JobSheet: View {
    @ObservedObject var work: WorkModel
    let jobID: String
    let fallback: WorkJob
    @Environment(\.dismiss) private var dismiss
    @State private var running: WorkAction?
    @State private var problem: String?
    @State private var confirmDrop = false

    /// The job as the last refresh has it, so an action's result shows here at once.
    private var job: WorkJob { work.jobs?.jobs.first { $0.id == jobID } ?? fallback }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(job.statusLine).font(.subheadline).foregroundStyle(.secondary)
                        if !job.steps.isEmpty {
                            StepLine(progress: job.progress)
                            Text("\(job.stepsDone) of \(job.steps.count) steps done")
                                .font(.footnote).foregroundStyle(.secondary)
                        }
                        if !job.endState.isEmpty {
                            Text(job.endState).font(.body).padding(.top, 4)
                        }
                    }
                    .padding(.vertical, 4)
                    .listRowBackground(Color.clear)
                }
                if !job.steps.isEmpty {
                    Section("Steps") {
                        ForEach(job.steps) { step in StepRow(step: step) }
                    }
                }
                if let usd = job.spentUSD, let budget = job.budgetUSD {
                    Section {
                        LabeledContent("Spent", value: usd.formatted(.currency(code: "USD")))
                        LabeledContent("Budget", value: budget.formatted(.currency(code: "USD")))
                    }
                }
                if let problem {
                    Section { Text(problem).foregroundStyle(.red).font(.subheadline) }
                }
                if !job.actions.isEmpty {
                    Section { actions }
                }
            }
            .navigationTitle(job.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
            .refreshable { await work.refreshJobs() }
            .confirmationDialog("Drop \u{201C}\(job.title)\u{201D}?", isPresented: $confirmDrop, titleVisibility: .visible) {
                Button("Drop job", role: .destructive) { Task { await run(.drop) } }
            }
        }
        .presentationDragIndicator(.visible)
        .sensoryFeedback(.success, trigger: work.acted)
    }

    @ViewBuilder private var actions: some View {
        ForEach(job.actions.filter { $0 != .drop && $0 != .hide }, id: \.self) { action in
            Button { Task { await run(action) } } label: {
                HStack {
                    Text(action.title).fontWeight(action == .go ? .semibold : .regular)
                    Spacer()
                    if running == action { ProgressView() }
                }
            }
            .disabled(running != nil)
            .accessibilityHint(hint(action))
        }
        if job.actions.contains(.drop) {
            Button("Drop", role: .destructive) { confirmDrop = true }
                .disabled(running != nil)
                .accessibilityHint(hint(.drop))
        }
        if job.actions.contains(.hide) {
            Button("Delete", role: .destructive) { Task { await run(.hide) } }
                .disabled(running != nil)
                .accessibilityHint(hint(.hide))
        }
    }

    private func hint(_ action: WorkAction) -> String {
        switch action {
        case .go: "Gives eve your Go for the next step"
        case .done: "Closes the job as done"
        case .drop: "Stops the job"
        case .raise: "Doubles the job's budget"
        case .happened: "Tells eve the step it may have done did happen"
        case .didnt: "Tells eve the step did not happen; it will need your Go again"
        case .hide: "Removes the finished job from this list"
        }
    }

    private func run(_ action: WorkAction) async {
        running = action
        problem = nil
        let said = await work.act(action, on: job)
        running = nil
        problem = said
        if said == nil, action == .done || action == .drop || action == .hide { dismiss() }
    }
}

private struct StepRow: View {
    let step: WorkStep

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: symbol)
                .foregroundStyle(tint)
                .imageScale(.small)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(step.goal.isEmpty ? step.kind.capitalized : step.goal).font(.body)
                Text(step.statusWord).font(.footnote).foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Step \(step.n). \(step.goal). \(step.statusWord)")
    }

    private var symbol: String {
        switch step.status {
        case "ok": "checkmark.circle.fill"
        case "running": "circle.dotted"
        case "failed", "refused": "xmark.circle"
        case "skipped": "arrow.right.circle"
        default: step.oneWay ? "hand.raised.circle" : "circle"
        }
    }

    private var tint: Color {
        switch step.status {
        case "ok": .green
        case "failed", "refused": .red
        case "queued" where step.oneWay: .blue
        default: .secondary
        }
    }
}
