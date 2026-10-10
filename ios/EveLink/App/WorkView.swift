// The Apps and Work tabs (docs/app-chat.md): one plain native list each. Work is the chief's jobs: a job opens a sheet
// with its steps and only the actions that apply, and its bar's "+" starts a New job (eve takes it on as in the
// chat). Apps is eve's own controls (Notes, the owner's vault, NotesView.swift; Music, Lights, Remote: the Mini App's
// cards; Pika, the car, PikaView.swift), each opening its native sheet (ControlSheets.swift). Memory is in Settings (owner, 2026-10-10). The owner's built apps are not listed (owner,
// 2026-10-08); the owner asked for "chat, apps, work" as three tabs the same evening.
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

    /// A New job: eve hears the goal as a chat turn and files the card itself (link_chat ``POST /api/link/jobs``).
    /// The card it filed, or the line the sheet shows (eve's question back, or why there is no card).
    func start(goal: String, detail: String, clientID: String) async -> Result<WorkJob, StartProblem> {
        let out: Result<WorkJob, StartProblem>
        switch await client.start(goal: goal, detail: detail, clientID: clientID) {
        case .success(let r):
            if let job = r.job {
                out = .success(job)
                acted += 1
            } else {
                out = .failure(StartProblem(message: r.problem))
            }
        case .failure(let f):
            out = .failure(StartProblem(message: f.message))
        }
        await refreshJobs()
        return out
    }

    struct StartProblem: Error { let message: String }

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
    @State private var control: ControlKind?
    @State private var newJob = false
    /// The job a New job filed: its sheet opens once the New job sheet has gone.
    @State private var started: WorkJob?

    var body: some View {
        VStack(spacing: 0) {
            if shows == .work { workBar }
            List {
                if shows == .apps { controlRows } else { jobRows }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
        }
        .background(Color.eveBackground)
        .refreshable { await reload() }
        .task(id: shows) {
            await reload()
            #if DEBUG && targetEnvironment(simulator)
            if shows == .apps, let raw = DesignHook.control ?? (DesignHook.screen == "pika" ? "pika" : nil),
               let kind = ControlKind(rawValue: raw) { control = kind }
            #endif
        }
        .sheet(item: $job) { shown in
            JobSheet(work: work, jobID: shown.id, fallback: shown)
        }
        .sheet(item: $control) { kind in
            ControlSheet(kind: kind, client: work.client, lights: work.controls?.lights ?? [])
        }
        .sheet(isPresented: $newJob, onDismiss: {
            if let filed = started {
                started = nil
                job = filed
            }
        }) {
            NewJobSheet(work: work) { filed in started = filed }
        }
    }

    /// The Work list's own toolbar: what it lists, and New job.
    private var workBar: some View {
        HStack {
            Text("Jobs").font(.headline)
            Spacer()
            Button { newJob = true } label: {
                Image(systemName: "plus").font(.system(size: 16, weight: .semibold))
            }
            .buttonStyle(RoundButtonStyle(size: 36))
            .foregroundStyle(.primary)
            .accessibilityLabel("New job")
            .accessibilityHint("Hands eve a job to take on")
        }
        .padding(.horizontal, 20)
        .padding(.top, 8)
        .padding(.bottom, 4)
    }

    private func reload() async {
        if shows == .apps { await work.refreshControls() } else { await work.refreshJobs() }
    }

    @ViewBuilder private var jobRows: some View {
        if let jobs = work.jobs, !jobs.jobs.isEmpty {
            ForEach(jobs.open + jobs.finished) { j in
                Button { job = j } label: { JobRow(job: j) }
                    .listRowBackground(Color.eveBackground)
            }
        } else {
            quiet(work.jobsProblem ?? (work.jobs == nil ? "Loading…" : "No jobs."))
        }
    }

    @ViewBuilder private var controlRows: some View {
        if let controls = work.controls, !controls.apps.isEmpty {
            ForEach(controls.apps) { kind in
                Button { control = kind } label: { ControlRow(kind: kind, info: controls) }
                    .listRowBackground(Color.eveBackground)
            }
        } else {
            quiet(work.controlsProblem ?? (work.controls == nil ? "Loading…" : "No notes, music, lights or TV on this Mac."))
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

/// One of eve's controls: its symbol in a rounded square, its name, and what it reaches.
private struct ControlRow: View {
    let kind: ControlKind
    let info: ControlsInfo

    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: kind.symbol)
                .font(.system(size: 17, weight: .medium))
                .foregroundStyle(.primary)
                .frame(width: 36, height: 36)
                .background(RoundedRectangle(cornerRadius: 9, style: .continuous).fill(Color.eveFill))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(kind.title).font(.body).foregroundStyle(.primary)
                Text(detail).font(.subheadline).foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(kind.title). \(detail)")
        .accessibilityHint("Opens the \(kind.title.lowercased()) controls")
        .accessibilityAddTraits(.isButton)
    }

    private var detail: String {
        switch kind {
        case .spotify: "Spotify"
        case .lights: info.lights.count == 1 ? "1 light" : "\(info.lights.count) lights"
        case .firetv: "Fire TV"
        case .memory: "What eve remembers"
        case .notes: "Your second brain"
        case .pika: "Drive the car"
        }
    }
}

// ---- a new job (owner, 2026-10-09: "work tab should allow me to start long horizon task not only just track") ------

/// The goal in the owner's words, optional details, and Start. eve takes it on as it would in the chat: a step that
/// cannot be undone still waits on the owner's Go in the job's sheet.
private struct NewJobSheet: View {
    @ObservedObject var work: WorkModel
    /// The card eve filed: the caller opens its sheet once this one has gone.
    let filed: (WorkJob) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var goal = ""
    @State private var detail = ""
    @State private var running = false
    @State private var problem: String?
    /// One per goal and details as typed: a retry after a dropped connection or a slow turn is the same job, never a
    /// second one; changed words are a new job.
    @State private var clientID = "job-" + UUID().uuidString.lowercased()
    @FocusState private var goalFocused: Bool

    private var canStart: Bool { !running && !goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    TextField("Plan my trip to Rome in December, flights and a hotel under $200 a night",
                              text: $goal, axis: .vertical)
                        .lineLimit(3...8)
                        .focused($goalFocused)
                        .accessibilityLabel("Goal")
                } header: {
                    Text("Goal")
                } footer: {
                    Text("eve plans the steps and asks for your Go before anything it can't undo.")
                }
                Section("Details") {
                    TextField("Optional: dates, limits, what to avoid", text: $detail, axis: .vertical)
                        .lineLimit(2...6)
                        .accessibilityLabel("Details, optional")
                }
                if let problem {
                    Section { Text(problem).foregroundStyle(.red).font(.subheadline) }
                }
            }
            .disabled(running)
            .navigationTitle("New job")
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
            .onAppear { goalFocused = true }
            .onChange(of: goal) { clientID = "job-" + UUID().uuidString.lowercased() }
            .onChange(of: detail) { clientID = "job-" + UUID().uuidString.lowercased() }
        }
        .presentationDragIndicator(.visible)
        .interactiveDismissDisabled(running)
        .sensoryFeedback(.success, trigger: work.acted)
    }

    private func start() async {
        guard canStart else { return }
        running = true
        problem = nil
        let out = await work.start(goal: String(goal.prefix(WorkRequests.maxGoal)),
                                   detail: String(detail.prefix(WorkRequests.maxDetail)), clientID: clientID)
        running = false
        switch out {
        case .success(let job):
            filed(job)
            dismiss()
        case .failure(let p):
            problem = p.message                 // the same words again wait for the same turn (clientID)
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
        ForEach(job.actions.filter { $0 != .drop }, id: \.self) { action in
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
    }

    private func hint(_ action: WorkAction) -> String {
        switch action {
        case .go: "Gives eve your Go for the next step"
        case .done: "Closes the job as done"
        case .drop: "Stops the job"
        case .raise: "Doubles the job's budget"
        case .happened: "Tells eve the step it may have done did happen"
        case .didnt: "Tells eve the step did not happen; it will need your Go again"
        }
    }

    private func run(_ action: WorkAction) async {
        running = action
        problem = nil
        let said = await work.act(action, on: job)
        running = nil
        problem = said
        if said == nil, action == .done || action == .drop { dismiss() }
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
