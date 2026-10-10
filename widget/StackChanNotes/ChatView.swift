// The chat window: eve's conversation with you, like the Telegram chat, with a
// reply box. Opened by tapping the widget (stackchan://chat) or from the menu.
//
// What it shows is eve's own transcript (bridge transcripts.py): one JSON line
// per thing said, in ~/.config/eve-bridge/memory/transcripts/<day>.jsonl.
// The window reads those files in place and never copies them, because the owner's
// rule (2026-09-23) keeps the words out of any backed-up folder, and the widget's
// App Group container is one. So the widget itself shows no words; it opens this.
//
// The reply box sends {"evt":"chat","text":..} on the daemon's socket (daemon.py
// _ipc_chat): the words go to the same brain as a Telegram text, with every tool,
// and the transcript then holds both them and eve's answer.

import SwiftUI

struct ChatMessage: Identifiable, Equatable {
    let id: String
    let time: Date
    let fromOwner: Bool
    let text: String
    let channel: String      // "telegram" or "voice"
}

@MainActor
@Observable
final class ChatStore {
    static let shared = ChatStore()

    private(set) var messages: [ChatMessage] = []
    private(set) var sending = false
    private(set) var waitingSince: Date?
    private(set) var error: String?

    /// How many days of transcript the window loads, newest last.
    private let days = 3
    private let maxMessages = 400
    private var stamps: [URL: Date] = [:]
    private var cache: [URL: [ChatMessage]] = [:]
    private var timer: Timer?

    static var transcriptsDir: URL {
        let env = ProcessInfo.processInfo.environment["EVE_MEMORY_DIR"]
        let memory = env.map { URL(fileURLWithPath: ($0 as NSString).expandingTildeInPath) }
            ?? FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".config/eve-bridge/memory", isDirectory: true)
        return memory.appendingPathComponent("transcripts", isDirectory: true)
    }

    func start() {
        reload()
        guard timer == nil else { return }
        timer = Timer.scheduledTimer(withTimeInterval: 1.5, repeats: true) { _ in
            Task { @MainActor in ChatStore.shared.reload() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    /// Re-reads only the day files whose modification date moved.
    func reload() {
        let fm = FileManager.default
        let names = ((try? fm.contentsOfDirectory(atPath: Self.transcriptsDir.path)) ?? [])
            .filter { $0.hasSuffix(".jsonl") }
            .sorted()
            .suffix(days)
        var changed = false
        var all: [ChatMessage] = []
        for name in names {
            let url = Self.transcriptsDir.appendingPathComponent(name)
            let stamp = (try? fm.attributesOfItem(atPath: url.path)[.modificationDate]) as? Date
            if stamp != stamps[url] || cache[url] == nil {
                cache[url] = Self.parse(url)
                stamps[url] = stamp
                changed = true
            }
            all += cache[url] ?? []
        }
        guard changed || all.count != messages.count else { return }
        let newest = Array(all.suffix(maxMessages))
        if let since = waitingSince, newest.contains(where: { !$0.fromOwner && $0.time >= since }) {
            waitingSince = nil
        }
        if let since = waitingSince, Date().timeIntervalSince(since) > 180 {
            waitingSince = nil
        }
        messages = newest
    }

    private static let isoFull: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()
    private static let iso: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()

    /// The lines that are conversation: things said on Telegram or by voice, and
    /// photos you sent. Tool calls, Claude relays, commands and close markers are
    /// eve's bookkeeping, not the chat.
    static func parse(_ url: URL) -> [ChatMessage] {
        guard let data = try? Data(contentsOf: url), let text = String(data: data, encoding: .utf8) else { return [] }
        var out: [ChatMessage] = []
        for (i, line) in text.split(separator: "\n", omittingEmptySubsequences: true).enumerated() {
            guard let obj = (try? JSONSerialization.jsonObject(with: Data(line.utf8))) as? [String: Any],
                  let ch = obj["ch"] as? String, ch == "telegram" || ch == "voice",
                  let kind = obj["kind"] as? String,
                  let who = obj["who"] as? String, who == "owner" || who == "eve"
            else { continue }
            let body: String
            switch kind {
            case "say": body = (obj["text"] as? String) ?? ""
            case "image": body = "📷 " + ((obj["text"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "a photo")
            default: continue
            }
            guard !body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { continue }
            let ts = (obj["ts"] as? String) ?? ""
            let time = isoFull.date(from: ts) ?? iso.date(from: ts) ?? .distantPast
            out.append(ChatMessage(id: "\(url.lastPathComponent)#\(i)", time: time, fromOwner: who == "owner",
                                   text: body, channel: ch))
        }
        return out
    }

    func send(_ text: String) async -> Bool {
        let words = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !words.isEmpty, !sending else { return false }
        sending = true
        error = nil
        defer { sending = false }
        let reply = await EveDaemon.post(["evt": "chat", "text": words])
        guard let reply else {
            error = "eve isn't running (no answer on its socket)"
            return false
        }
        guard reply["ok"] as? Bool == true else {
            error = (reply["error"] as? String) ?? "eve didn't take that"
            return false
        }
        waitingSince = Date()
        reload()
        return true
    }
}

struct ChatView: View {
    @State private var store = ChatStore.shared
    @State private var draft = ""
    @FocusState private var typing: Bool

    var body: some View {
        VStack(spacing: 0) {
            header
            Rectangle().fill(Color.brandInk).frame(height: 2)
            conversation
            Rectangle().fill(Color.brandInk).frame(height: 2)
            composer
        }
        .frame(minWidth: 380, minHeight: 460)
        .background(Color.brandPaper)
        .foregroundStyle(Color.brandInk)
        .tint(Color.brandInk)
        .environment(\.colorScheme, .light)
        .onAppear {
            store.start()
            typing = true
        }
        .onDisappear { store.stop() }
    }

    private var header: some View {
        HStack(spacing: 12) {
            RobotFace(size: 40, compact: true)
            VStack(alignment: .leading, spacing: 1) {
                InkHeading("eve", size: 22)
                Text(status)
                    .font(BrandFont.body(12))
                    .foregroundStyle(Color.brandInkSoft)
                    .lineLimit(1)
            }
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
    }

    private var status: String {
        if store.waitingSince != nil { return "thinking…" }
        if let last = store.messages.last {
            return "last message \(last.time.formatted(date: .omitted, time: .shortened))"
        }
        return "the same chat as Telegram"
    }

    private var conversation: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    if store.messages.isEmpty {
                        Text("Nothing said yet. Type below, text eve on Telegram, or hold the Voice PE's button.")
                            .font(BrandFont.body(13))
                            .foregroundStyle(Color.brandInkSoft)
                            .frame(maxWidth: .infinity)
                            .padding(.top, 40)
                    }
                    ForEach(Array(store.messages.enumerated()), id: \.element.id) { i, m in
                        if i == 0 || !Calendar.current.isDate(m.time, inSameDayAs: store.messages[i - 1].time) {
                            DayDivider(date: m.time)
                        }
                        Bubble(message: m)
                    }
                    if store.waitingSince != nil {
                        TypingBubble()
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(14)
            }
            .onChange(of: store.messages.count) { _, _ in
                withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .onChange(of: store.waitingSince) { _, _ in proxy.scrollTo("bottom", anchor: .bottom) }
            .onAppear { proxy.scrollTo("bottom", anchor: .bottom) }
        }
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let error = store.error {
                Text(error)
                    .font(BrandFont.body(12))
                    .foregroundStyle(Color(hex: 0xB3261E))
            }
            HStack(alignment: .bottom, spacing: 10) {
                TextField("Message eve", text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(BrandFont.body(14))
                    .lineLimit(1...6)
                    .focused($typing)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .paperCard(shadow: .brandPinkSoft, offset: 2, radius: 14, lineWidth: 1.5)
                    .onSubmit(sendDraft)
                    .disabled(store.sending)
                Button(action: sendDraft) {
                    Label("Send", systemImage: "paperplane.fill")
                }
                .buttonStyle(ChunkyButtonStyle())
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || store.sending)
                .help("Send (Return, or ⌘Return)")
            }
        }
        .padding(12)
    }

    private func sendDraft() {
        let words = draft
        guard !words.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        Task {
            if await store.send(words) {
                draft = ""
            }
            typing = true
        }
    }
}

private struct Bubble: View {
    let message: ChatMessage

    var body: some View {
        HStack(alignment: .bottom) {
            if message.fromOwner { Spacer(minLength: 60) }
            VStack(alignment: message.fromOwner ? .trailing : .leading, spacing: 3) {
                Text(rendered)
                    .font(BrandFont.body(14))
                    .foregroundStyle(Color.brandInk)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .paperCard(shadow: message.fromOwner ? .brandPink : .brandTeal, offset: 3, radius: 14,
                               fill: message.fromOwner ? .brandPinkSoft : .brandSheet, lineWidth: 1.5)
                Text(meta)
                    .font(BrandFont.body(10.5))
                    .foregroundStyle(Color.brandInkSoft)
            }
            if !message.fromOwner { Spacer(minLength: 60) }
        }
    }

    /// eve writes Markdown (bold, links, code); show it the way Telegram does.
    private var rendered: AttributedString {
        let opts = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: message.text, options: opts)) ?? AttributedString(message.text)
    }

    private var meta: String {
        let time = message.time.formatted(date: .omitted, time: .shortened)
        return message.channel == "voice" ? "🎙 \(time)" : time
    }
}

private struct TypingBubble: View {
    @State private var phase = 0.0

    var body: some View {
        HStack {
            Text("eve is thinking…")
                .font(BrandFont.body(13))
                .foregroundStyle(Color.brandInkSoft)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .paperCard(shadow: .brandTeal, offset: 3, radius: 14, lineWidth: 1.5)
                .opacity(0.55 + 0.45 * phase)
                .onAppear {
                    withAnimation(.easeInOut(duration: 0.8).repeatForever()) { phase = 1 }
                }
            Spacer()
        }
    }
}

private struct DayDivider: View {
    let date: Date

    var body: some View {
        HStack {
            Spacer()
            BrandPill(label, fill: .brandSunWash)
            Spacer()
        }
        .padding(.vertical, 4)
    }

    private var label: String {
        if Calendar.current.isDateInToday(date) { return "today" }
        if Calendar.current.isDateInYesterday(date) { return "yesterday" }
        return date.formatted(.dateTime.weekday(.wide).month().day())
    }
}
