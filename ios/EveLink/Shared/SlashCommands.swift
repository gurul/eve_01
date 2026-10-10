// The chat composer's / menu (docs/app-chat.md, "The composer"): eve's code words, as Telegram's / menu offers them.
// The daemon lists the ones it runs now (link_chat.py, TelegramInlet.menu_commands: /jobs only while the chief is on);
// a tapped one is sent as the turn's text, "/apps", which eve handles exactly as it does typed. Foundation only, so
// Tests/main.swift checks the matching, the decoding and the request on the Mac.
//
//   GET /api/link/commands     {"commands": [{"command": "apps", "description": "Your apps, and build a new one"}]}
import Foundation

/// One code word: `command` without its "/", and the line Telegram's menu shows under it.
struct SlashCommand: Equatable, Sendable, Identifiable, Hashable {
    var command: String
    var description: String
    var id: String { command }

    /// What is sent when the row is tapped.
    var text: String { "/" + command }
}

enum SlashMenu {
    /// The words every eve has (telegram.BOT_COMMANDS), for a daemon that does not answer the list or is older
    /// than it: the menu is never empty. A word the daemon adds (/jobs and the others) comes from its list only.
    static let builtIn: [SlashCommand] = [
        SlashCommand(command: "apps", description: "Your apps, and build a new one"),
        SlashCommand(command: "stick", description: "Pair Eve Link, the phone app"),
        SlashCommand(command: "spend", description: "What eve has spent today and this month"),
        SlashCommand(command: "claude_on", description: "Join a running Claude Code session"),
        SlashCommand(command: "claude_off", description: "Stop relaying Claude Code"),
        SlashCommand(command: "new_claude", description: "Open a new Claude Code session"),
        SlashCommand(command: "codex", description: "Chat with Codex in a folder"),
        SlashCommand(command: "rundown", description: "Mail, calendar and todos in one brief"),
        SlashCommand(command: "watch", description: "Watch a price, stock or ticket release, or list them"),
        SlashCommand(command: "meet", description: "Join a Google Meet call and take notes (a link, or 6pm)"),
        SlashCommand(command: "screenshot", description: "Send the Mac's screen"),
        SlashCommand(command: "stealth", description: "Act asleep at the desk"),
        SlashCommand(command: "wake", description: "Wake up from stealth"),
        SlashCommand(command: "stop", description: "Stop the running task"),
    ]

    /// What the owner has typed after the "/", while the menu should show: the field starts with "/" and has no
    /// space or line break yet. nil otherwise (no "/", or a space typed: the owner is writing an argument).
    static func query(_ draft: String) -> String? {
        guard draft.hasPrefix("/") else { return nil }
        let rest = draft.dropFirst()
        guard !rest.contains(where: \.isWhitespace) else { return nil }
        return rest.lowercased()
    }

    /// The rows for this draft: the words starting with what was typed, in the daemon's order. nil when the menu is
    /// hidden; empty when nothing matches (the menu hides then too).
    static func matches(_ commands: [SlashCommand], draft: String) -> [SlashCommand]? {
        guard let q = query(draft) else { return nil }
        return q.isEmpty ? commands : commands.filter { $0.command.hasPrefix(q) }
    }

    /// The daemon's list; nil for an answer that is not one. Entries that are not a Bot API command (1-32 lowercase
    /// letters, digits and underscores) are left out, as are repeats.
    static func decode(_ data: Data) -> [SlashCommand]? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let raw = obj["commands"] as? [[String: Any]] else { return nil }
        var seen = Set<String>()
        return raw.compactMap { c in
            guard let command = c["command"] as? String, valid(command), seen.insert(command).inserted else { return nil }
            return SlashCommand(command: command, description: c["description"] as? String ?? "")
        }
    }

    static func valid(_ command: String) -> Bool {
        (1...32).contains(command.count)
            && command.unicodeScalars.allSatisfy { ("a"..."z").contains($0) || ("0"..."9").contains($0) || $0 == "_" }
    }
}

extension WorkRequests {
    static func commands(_ ep: ChatEndpoint) -> URLRequest { request(ep, ["api", "link", "commands"], post: false) }
}

extension WorkClient {
    /// The daemon's / menu. An empty list counts as a failure, so the caller keeps the built-in one.
    func commands() async -> Result<[SlashCommand], ChatFailure> {
        await call(.commands, WorkRequests.commands) { data, _ in
            SlashMenu.decode(data).flatMap { $0.isEmpty ? nil : $0 }
        }
    }
}
