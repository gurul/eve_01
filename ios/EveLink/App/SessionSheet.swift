// One Claude Code session from the Work tab (docs/work-sessions.md), whatever terminal it runs in: its latest turns,
// read again every few seconds while the sheet is open, one message box, and Stop (with a confirm) for a session eve
// started. The box takes as many messages in a row as the owner types (owner, 2026-10-26: "I want to also be able to
// talk to current terminal sessions through the work area ... Unify everything"): each shows at once as the owner's
// bubble with a small line saying how it landed, and they go in order from WorkModel's outbox, never waiting on each
// other. A session that has ended gets Resume in the same box: the same session opened again on the Mac, under the
// same id, so the sheet stays on it. Nothing here forks a session or opens a second one.
import SwiftUI
import UIKit

struct SessionSheet: View {
    @ObservedObject var work: WorkModel
    /// The session as the list had it, until the first read.
    let fallback: WorkSession
    @Environment(\.dismiss) private var dismiss
    @State private var loadProblem: String?
    @State private var draft = ""
    @State private var stopping = false
    @State private var problem: String?
    @State private var confirmStop = false
    @FocusState private var draftFocused: Bool

    /// How often an open sheet reads its session again; an ended one more slowly (it can be resumed at the desk too).
    static let poll: Duration = .seconds(4)
    static let endedPoll: Duration = .seconds(12)

    private var id: String { fallback.id }
    private var detail: WorkSessionDetail? { work.details[id] }
    /// The newest the phone knows: the last read, with the newest state heard (a refused reply, a resume).
    private var session: WorkSession {
        var s = detail?.session ?? fallback
        if let state = work.heard[id] { s.state = state }
        return s
    }
    private var ended: Bool { !session.state.live }
    private var turns: [WorkSessionTurn] { detail?.turns ?? [] }
    private var outbox: WorkOutbox { work.outboxes[id] ?? WorkOutbox() }
    private var composer: WorkComposer { .of(session, resuming: outbox.resuming) }
    private var canSend: Bool { !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    /// Changes when a turn comes, the last one grows, or the owner sends: the list follows it down.
    private var scrollKey: String {
        "\(turns.count)-\(turns.last?.text.count ?? 0)-\(outbox.messages.count)-\(outbox.messages.last?.id ?? 0)"
    }

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
                            let captions = outbox.captions(failedHint: composer == .resume ? "Tap to put it back in the box."
                                                                                          : "Tap to try again.")
                            ForEach(outbox.messages) { m in
                                PendingView(message: m, caption: captions[m.id],
                                            tapHint: composer == .resume ? "Puts your words back in the box"
                                                                         : "Sends it again",
                                            tap: { tapped(m) }, delete: { work.discard(m.id, from: id) })
                                    .id("pending-\(m.id)")
                            }
                            if turns.isEmpty && outbox.isEmpty {
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
                bottom
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
            .task(id: ended) {               // starts again when it ends or comes back, so a resume reads at once
                while !Task.isCancelled {
                    let said = await work.load(id)
                    if Task.isCancelled { break }            // a read cut short by the restart is not a problem
                    loadProblem = said
                    try? await Task.sleep(for: ended ? Self.endedPoll : Self.poll)
                }
            }
        }
        .presentationDragIndicator(.visible)
        .sensoryFeedback(.success, trigger: work.acted)
    }

    /// The state chip, where it runs and its folder, and when it last moved.
    private var header: some View {
        TimelineView(.periodic(from: .now, by: 30)) { clock in
            HStack(spacing: 10) {
                SessionChip(state: session.state)
                if !session.place.isEmpty {
                    Label(session.place, systemImage: "folder")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .accessibilityLabel(session.placeSpoken)
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

    /// The message box for a live session, Resume in the same box for an ended one, or why neither. The box is one
    /// view either way, so the keyboard stays up when the session ends or comes back while the owner types.
    @ViewBuilder private var bottom: some View {
        if case .note(let line) = composer {
            note(line)
        } else {
            let resume = composer == .resume
            VStack(spacing: 0) {
                if resume {
                    Text("This session has ended. Resume opens it again in Terminal on your Mac, your words its next prompt.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 16)
                        .padding(.top, 8)
                }
                input(resume ? "Resume with a message" : "Message Claude", button: resume ? "Resume" : "Send",
                      hint: resume ? "Opens this same session again in Terminal on your Mac, your words its next prompt"
                            : session.runsIn.isEmpty ? "Sends your message to this session on your Mac"
                            : "Sends your message to this session in \(session.runsIn) on your Mac")
            }
        }
    }

    /// The text box and its button. It stays open while messages are on their way: the owner can send another at
    /// once, and it goes after the ones before it.
    private func input(_ placeholder: String, button: String, hint: String) -> some View {
        HStack(alignment: .bottom, spacing: 10) {
            TextField(placeholder, text: $draft, axis: .vertical)
                .lineLimit(1...5)
                .focused($draftFocused)
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(RoundedRectangle(cornerRadius: 20, style: .continuous).fill(Color.eveFill))
                .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous)
                    .strokeBorder(Color.eveEdge, lineWidth: 1))
                .accessibilityLabel("Message")
                .onChange(of: draft) { _, now in
                    if now.count > WorkSessionRequests.maxReply {
                        draft = String(now.prefix(WorkSessionRequests.maxReply))
                    }
                }
            Button(action: submit) {
                Image(systemName: "arrow.up").font(.system(size: 16, weight: .semibold))
            }
            .buttonStyle(RoundButtonStyle(size: 38))
            .foregroundStyle(.primary)
            .disabled(!canSend)
            .opacity(canSend ? 1 : 0.4)
            .accessibilityLabel(button)
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
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
    }

    private var emptyLine: String {
        if let loadProblem { return loadProblem }
        if detail == nil { return "Loading…" }
        return session.state == .starting ? "Claude is starting…" : "Nothing yet."
    }

    /// The words go to the outbox and the box empties at once; the bubble says how they landed.
    private func submit() {
        let words = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !words.isEmpty else { return }
        switch composer {
        case .message: work.send(words, to: id, shown: turns)
        case .resume: work.resume(id, with: words, shown: turns)
        case .note: return
        }
        draft = ""
    }

    /// A tap on a message that did not go: send it again, or, once the session has ended, put its words (and those of
    /// any behind it) back in the box for Resume.
    private func tapped(_ m: WorkOutbox.Message) {
        guard case .failed = m.phase else { return }
        if composer == .resume {
            let words = work.takeUnsent(id)
            let typed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
            draft = [words, typed].filter { !$0.isEmpty }.joined(separator: "\n\n")
            draftFocused = true
        } else {
            work.retry(id)
        }
    }

    private func stop() async {
        stopping = true
        problem = nil
        let said = await work.stop(id)
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
                OwnerBubble(text: turn.text)
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

/// The owner's words in their bubble.
private struct OwnerBubble: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.body)
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .background(RoundedRectangle(cornerRadius: 18, style: .continuous).fill(Color.eveFill))
    }
}

/// A message the owner sent that the transcript does not show yet: their bubble, faded until it has gone, and the
/// small line under it ("Sending…", "Sent", "Waiting for approval on the Mac"). One that failed has a red mark and
/// takes a tap; a long press copies it, or deletes one that has not gone.
private struct PendingView: View {
    let message: WorkOutbox.Message
    /// nil when the bubble below says the same.
    let caption: String?
    let tapHint: String
    let tap: () -> Void
    let delete: () -> Void

    private var failed: Bool { if case .failed = message.phase { true } else { false } }
    private var gone: Bool { if case .sent = message.phase { true } else { false } }

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            HStack(spacing: 8) {
                Spacer(minLength: 48)
                if failed {
                    Image(systemName: "exclamationmark.circle.fill")
                        .foregroundStyle(.red)
                        .accessibilityHidden(true)
                }
                OwnerBubble(text: message.text)
                    .opacity(gone || failed ? 1 : 0.6)
            }
            if let caption {
                Text(caption)
                    .font(.caption)
                    .foregroundStyle(failed ? Color.red : Color.secondary)
                    .multilineTextAlignment(.trailing)
            }
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .contentShape(Rectangle())
        .onTapGesture { if failed { tap() } }
        .contextMenu {
            Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = message.text }
            if message.phase.unsent {
                Button("Delete", systemImage: "trash", role: .destructive) { delete() }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("You: \(message.text)")
        .accessibilityValue(WorkOutbox.caption(message.phase))
        .accessibilityHint(failed ? tapHint : "")
        .accessibilityAddTraits(failed ? .isButton : [])
        .accessibilityAction { if failed { tap() } }
        .accessibilityActions {
            if message.phase.unsent { Button("Delete", action: delete) }
        }
    }
}
