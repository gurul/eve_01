// One Claude Code session from the Work tab (docs/work-sessions.md): its latest turns, read again every few seconds
// while the sheet is open, a reply box that types into that session's own terminal, and Stop (with a confirm) that
// ends it. A session the phone can't reply to (eve didn't start it, or it ended) can be continued: a fork of it opens as
// a new Work session in the same folder, the owner's words its next prompt, and the sheet follows the fork.
import SwiftUI

struct SessionSheet: View {
    @ObservedObject var work: WorkModel
    /// The session as the list had it, until the first read.
    let fallback: WorkSession
    @Environment(\.dismiss) private var dismiss
    /// The session the sheet shows: the one it opened on, then the fork a continue made.
    @State private var current: String
    /// The fork as the continue answered it, until its first read.
    @State private var forked: WorkSession?
    /// One per continue wording, so a retry after a dropped connection is the same fork.
    @State private var continueID = UUID().uuidString
    @State private var detail: WorkSessionDetail?
    @State private var loadProblem: String?
    @State private var ended = false
    @State private var reply = ""
    @State private var sending = false
    @State private var stopping = false
    @State private var problem: String?
    @State private var confirmStop = false
    @FocusState private var replyFocused: Bool

    /// How often an open sheet reads its session again.
    static let poll: Duration = .seconds(4)

    init(work: WorkModel, fallback: WorkSession) {
        self.work = work
        self.fallback = fallback
        _current = State(initialValue: fallback.id)
    }

    private var session: WorkSession {
        var s = detail?.session ?? forked ?? fallback
        if ended { s.state = .ended }
        return s
    }
    private var turns: [WorkSessionTurn] { detail?.turns ?? [] }
    private var canSend: Bool {
        !sending && !reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
    /// Changes when a turn comes or the last one grows: the list follows it down.
    private var scrollKey: String { "\(turns.count)-\(turns.last?.text.count ?? 0)" }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                header
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 14) {
                            if detail?.cut == true {
                                Text("Earlier turns aren't shown.")
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                                    .frame(maxWidth: .infinity)
                            }
                            ForEach(turns) { TurnView(turn: $0) }
                            if turns.isEmpty {
                                Text(emptyLine)
                                    .font(.subheadline)
                                    .foregroundStyle(.secondary)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            Color.clear.frame(height: 1).id("end")
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 12)
                    }
                    .scrollDismissesKeyboard(.interactively)
                    .onChange(of: scrollKey, initial: true) {
                        withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("end", anchor: .bottom) }
                    }
                }
                if let problem {
                    Text(problem)
                        .font(.subheadline)
                        .foregroundStyle(.red)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 16)
                        .padding(.top, 6)
                }
                replyBar
            }
            .background(Color.eveBackground)
            .navigationTitle(session.headline)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                if session.canStop && !ended {
                    ToolbarItem(placement: .cancellationAction) {
                        if stopping {
                            ProgressView().accessibilityLabel("Stopping")
                        } else {
                            Button("Stop", role: .destructive) { confirmStop = true }
                                .foregroundStyle(.red)
                                .accessibilityHint("Ends this Claude Code session on your Mac")
                        }
                    }
                }
            }
            .confirmationDialog("Stop this session?", isPresented: $confirmStop, titleVisibility: .visible) {
                Button("Stop session", role: .destructive) { Task { await stop() } }
            } message: {
                Text("Claude Code quits on your Mac. Its terminal window stays open.")
            }
            .task(id: current) {
                while !Task.isCancelled {
                    await load()
                    if ended { break }
                    try? await Task.sleep(for: Self.poll)
                }
            }
        }
        .presentationDragIndicator(.visible)
        .sensoryFeedback(.success, trigger: work.acted)
    }

    /// The state chip, the folder and when it last moved.
    private var header: some View {
        TimelineView(.periodic(from: .now, by: 30)) { clock in
            HStack(spacing: 10) {
                SessionChip(state: session.state)
                if !session.folder.isEmpty {
                    Label(session.folder, systemImage: "folder")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
                Text(WorkSession.ago(session.lastActivity, now: clock.date))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .accessibilityElement(children: .combine)
        }
    }

    @ViewBuilder private var replyBar: some View {
        if !ended && session.canReply {
            input("Reply to Claude", hint: "Types your reply into this session on your Mac") { await send() }
        } else if session.canContinue {
            VStack(spacing: 0) {
                input("Continue from here", hint: "Opens this conversation in a new Terminal window on your Mac") {
                    await continueHere()
                }
                Text(ended ? "This session has ended. Continuing opens it again in a new Terminal window on your Mac."
                           : "Continuing opens this conversation in a new Terminal window on your Mac. The original stays as it is.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
            }
        } else if ended {
            note("This session has ended.")
        } else {
            note(session.fromWork ? "This session can't take a reply from the phone."
                                  : "eve can't find this session's transcript, so it can only show it here.")
        }
    }

    /// The text box and its send button, for a reply or a continue.
    private func input(_ placeholder: String, hint: String, action: @escaping () async -> Void) -> some View {
        HStack(alignment: .bottom, spacing: 10) {
            TextField(placeholder, text: $reply, axis: .vertical)
                .lineLimit(1...5)
                .focused($replyFocused)
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(RoundedRectangle(cornerRadius: 20, style: .continuous).fill(Color.eveFill))
                .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous)
                    .strokeBorder(Color.eveEdge, lineWidth: 1))
                .disabled(sending)
                .accessibilityLabel("Reply")
                .onChange(of: reply) { _, now in
                    if now.count > WorkSessionRequests.maxReply {
                        reply = String(now.prefix(WorkSessionRequests.maxReply))
                    }
                }
            Button { Task { await action() } } label: {
                if sending {
                    ProgressView()
                } else {
                    Image(systemName: "arrow.up").font(.system(size: 16, weight: .semibold))
                }
            }
            .buttonStyle(RoundButtonStyle(size: 38))
            .foregroundStyle(.primary)
            .disabled(!canSend)
            .opacity(canSend || sending ? 1 : 0.4)
            .accessibilityLabel(sending ? "Sending" : "Send")
            .accessibilityHint(hint)
        }
        .padding(.horizontal, 12)
        .padding(.top, 8)
        .padding(.bottom, 10)
    }

    private func note(_ line: String) -> some View {
        Text(line)
            .font(.footnote)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 14)
    }

    private var emptyLine: String {
        if let loadProblem { return loadProblem }
        if detail == nil { return "Loading…" }
        return session.state == .starting ? "Claude is starting…" : "Nothing yet."
    }

    private func load() async {
        switch await work.detail(current) {
        case .success(let d):
            if d != detail { detail = d }
            loadProblem = nil
            if d.session.state == .ended { ended = true }
        case .failure(let p):
            if p.message == WorkFailure.sessionEnded { ended = true }
            loadProblem = p.message
        }
    }

    private func send() async {
        guard canSend else { return }
        sending = true
        problem = nil
        let said = await work.reply(reply, to: current)
        sending = false
        if let said {
            problem = said
            if said == WorkFailure.sessionEnded { ended = true }
        } else {
            reply = ""
            await load()
        }
    }

    /// Forks this session into a Work session and follows it: the sheet now reads, replies to and stops the fork.
    private func continueHere() async {
        guard canSend else { return }
        sending = true
        problem = nil
        let result = await work.continueSession(reply, from: current, clientID: continueID)
        sending = false
        switch result {
        case .success(let s):
            reply = ""
            continueID = UUID().uuidString
            forked = s
            detail = nil
            loadProblem = nil
            ended = false
            current = s.id                  // the .task restarts on the fork
        case .failure(let p):
            problem = p.message
        }
    }

    private func stop() async {
        stopping = true
        problem = nil
        let said = await work.stop(current)
        stopping = false
        if let said {
            problem = said
        } else {
            dismiss()
        }
    }
}

/// One turn: the owner's words in a bubble on the right, Claude's as plain text on the left (inline Markdown only).
private struct TurnView: View {
    let turn: WorkSessionTurn

    var body: some View {
        if turn.role == .owner {
            HStack {
                Spacer(minLength: 48)
                Text(turn.text)
                    .font(.body)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 9)
                    .background(RoundedRectangle(cornerRadius: 18, style: .continuous).fill(Color.eveFill))
                    .textSelection(.enabled)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("You: \(turn.text)")
        } else {
            Text(rendered)
                .font(.body)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
                .accessibilityLabel("Claude: \(turn.text)")
        }
    }

    private var rendered: AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: turn.text, options: options)) ?? AttributedString(turn.text)
    }
}
